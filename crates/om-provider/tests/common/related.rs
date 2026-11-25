//! Hosted files whose table occurrences show other hosted files' tables
//! (file-metadata.md 1.1), served together by one `om-server` on verified
//! loopback TLS, and what its recordings say the provider sent. Every file
//! is a synthetic Tasks file (`om-server/tests/fixtures/tasks.rs`), so the
//! files' own table, field, record and occurrence ids coincide.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use om_data::{Connection, DataProvider};
use om_hbam::slot::uint_segment;
use om_hbam::OwnedNode;
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerHandle, ServerIdentity};

use crate::fixture::{self, TempFile, ACCOUNT, PASSWORD, TABLE};

/// A key string `[u8 total][u8 len][segment]` (file-metadata.md 0).
pub fn key(id: u64) -> Vec<u8> {
    let segment = uint_segment(id);
    let mut out = vec![segment.len() as u8 + 1, segment.len() as u8];
    out.extend(segment);
    out
}

/// Make occurrence `occurrence`, named `name`, show the Tasks table of
/// the file data source `source` names by `alternatives`, (scheme,
/// location, file) each, the source itself being named `source_name`.
pub fn reference_with(
    root: &mut OwnedNode,
    (occurrence, name): (u64, &str),
    source: u64,
    source_name: &str,
    alternatives: &[(&str, &str, &str)],
) {
    fixture::add_occurrence(root, occurrence, name, TABLE);
    let mut definition = key(source);
    definition.extend(key(TABLE));
    root.put(
        &[&[3], &[17], &[5], &uint_segment(occurrence), &[2]],
        OwnedNode::leaf(Vec::new(), definition),
    );
    root.put(
        &[&[32], &[5], &uint_segment(source), &[0x10]],
        om_scripts::text_node(&[], source_name),
    );
    let mut value = (alternatives.len() as u32).to_be_bytes().to_vec();
    for (scheme, location, file) in alternatives {
        for text in [scheme, location, file] {
            value.extend(om_scsu::encode_counted(text));
        }
    }
    root.put(
        &[&[32], &[5], &uint_segment(source), &[0x82]],
        OwnedNode::leaf(Vec::new(), value),
    );
}

/// [`reference_with`] a relative reference to `file`, as FileMaker stores
/// a data source naming a file on the same host.
pub fn reference(root: &mut OwnedNode, (occurrence, name): (u64, &str), source: u64, file: &str) {
    reference_with(
        root,
        (occurrence, name),
        source,
        file,
        &[("file", "", file)],
    );
}

/// A server hosting files by name, with its connections recorded.
pub struct Host {
    pub port: u16,
    pub handle: ServerHandle,
    ca_pem: Vec<u8>,
    pub captures: Captures,
    files: Vec<TempFile>,
}

impl Host {
    /// Host `files` (name with extension, tree) for the Admin account,
    /// closing connections silent for `idle_timeout`.
    pub fn with_idle_timeout(
        tag: &str,
        files: Vec<(&str, OwnedNode)>,
        idle_timeout: Option<Duration>,
    ) -> Host {
        Self::configured(tag, files, |server| match idle_timeout {
            Some(timeout) => server.with_idle_timeout(timeout),
            None => server,
        })
    }

    /// Host `files` for the Admin account on a server `configure` changes.
    pub fn configured(
        tag: &str,
        files: Vec<(&str, OwnedNode)>,
        configure: impl FnOnce(Server) -> Server,
    ) -> Host {
        Self::bound(tag, files, "127.0.0.1:0", configure).unwrap()
    }

    /// Host the files `files` gives for the port they are hosted on: files
    /// whose data sources name their own host's port. A free port is
    /// chosen first, and another if a process takes it before the server
    /// binds it.
    pub fn naming_its_port(
        tag: &str,
        files: impl Fn(u16) -> Vec<(&'static str, OwnedNode)>,
    ) -> Host {
        for _ in 0..8 {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            if let Some(host) = Self::bound(tag, files(port), &format!("127.0.0.1:{port}"), |s| s) {
                return host;
            }
        }
        panic!("no free port for {tag}");
    }

    /// Host `files` at `port` with the identity `ca` made beforehand, so a
    /// client could trust the host before it was up; `None` when the port
    /// cannot be bound.
    pub fn serving_at(
        tag: &str,
        files: Vec<(&str, OwnedNode)>,
        port: u16,
        ca: &Ca,
    ) -> Option<Host> {
        Self::bound_as(tag, files, &format!("127.0.0.1:{port}"), |s| s, Some(ca))
    }

    /// Host `files` at `addr`, `None` when the address cannot be bound.
    fn bound(
        tag: &str,
        files: Vec<(&str, OwnedNode)>,
        addr: &str,
        configure: impl FnOnce(Server) -> Server,
    ) -> Option<Host> {
        Self::bound_as(tag, files, addr, configure, None)
    }

    /// [`Self::bound`], with the identity `ca` when given.
    fn bound_as(
        tag: &str,
        files: Vec<(&str, OwnedNode)>,
        addr: &str,
        configure: impl FnOnce(Server) -> Server,
        ca: Option<&Ca>,
    ) -> Option<Host> {
        let captures = Captures::new(tag);
        let files: Vec<(String, TempFile)> = files
            .into_iter()
            .map(|(name, tree)| {
                let path = TempFile::from_tree(&format!("{tag}-{name}"), &tree);
                (name.to_owned(), path)
            })
            .collect();
        let ca = ca.cloned().unwrap_or_else(Ca::new);
        let ca_pem = ca.cert_pem.clone();
        let identity = Identity::from_pem(&ca_pem, &ca.key_pem).unwrap();
        let config = ServerConfig {
            identity: ServerIdentity {
                name: format!("related-{tag}"),
                version_string: "OpenMaker test".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            files: files
                .iter()
                .map(|(name, file)| HostedFile {
                    name: name.clone(),
                    path: file.0.clone(),
                })
                .collect(),
            insecure_files: false,
            record_dir: Some(captures.0.clone()),
        };
        // Accounts in custom sets too: these tests are about what the client
        // does with an account's privilege metadata (#734).
        let server = configure(
            Server::bind(addr, config, identity)
                .ok()?
                .admitting_custom_sets(),
        );
        // Every download and find is answered through the test view too, and
        // checked against the file engine's view (docs/server-storage.md 5,
        // "Views").
        #[cfg(debug_assertions)]
        let server = server.checking_reads();
        let port = server.local_addr().unwrap().port();
        let handle = server.handle().unwrap();
        std::thread::spawn(move || server.serve());
        Some(Host {
            port,
            handle,
            ca_pem,
            captures,
            files: files.into_iter().map(|(_, file)| file).collect(),
        })
    }

    pub fn new(tag: &str, files: Vec<(&str, OwnedNode)>) -> Host {
        Self::with_idle_timeout(tag, files, None)
    }

    /// A directory at the synthetic file's save destination makes op70 fail
    /// with a system exception on a commit that storage refuses. Remove it
    /// to allow retry.
    pub fn block_saves(&self, file: usize) -> SaveBarrier {
        let mut destination = self.files[file].0.as_os_str().to_owned();
        destination.push(".omsave");
        let destination = PathBuf::from(destination);
        std::fs::create_dir(&destination).unwrap();
        SaveBarrier(destination)
    }

    /// Make every commit's save of file `file` fail while the returned
    /// guard lives, as storage refusing it would, while its other saves
    /// (op71's issued record ids, #927) work: op70 fails with a system
    /// exception on a commit whose record ids the file already holds as
    /// issued.
    pub fn block_commit_saves(&self, file: usize) -> CommitBarrier<'_> {
        let path = std::path::absolute(&self.files[file].0).unwrap();
        let id = self
            .handle
            .files()
            .into_iter()
            .find(|hosted| hosted.path == path)
            .expect("a hosted file")
            .id;
        assert!(
            self.handle.failing_commit_saves(id, true),
            "the file is open"
        );
        CommitBarrier(&self.handle, id)
    }

    /// A provider trusting this host and `other`, not connected yet: one
    /// that may open files on both.
    pub fn provider_trusting(&self, other: &Host) -> NativeProvider {
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        NativeProvider::new(NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.key_pair.serialize_pem().into_bytes(),
            additional_ca_pem: [self.ca_pem.as_slice(), other.ca_pem.as_slice()].concat(),
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        })
    }

    /// Hosted file `name` replaced by `tree`, as an administrator replaces
    /// a file: closed once nothing holds it, written anew and opened again
    /// (`ServerHandle::close_file`, `open_file`).
    pub fn replace_file(&self, name: &str, tree: &OwnedNode) {
        let file = self
            .handle
            .files()
            .into_iter()
            .find(|file| file.name == name)
            .expect("a hosted file");
        let status = |want: om_server::FileStatus| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if self
                    .handle
                    .files()
                    .iter()
                    .any(|f| f.id == file.id && f.status == want)
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("{name} not {want:?}");
        };
        self.handle
            .close_file(file.id, &om_server::Notice::default())
            .unwrap();
        status(om_server::FileStatus::Closed);
        fixture::write_tree(&file.path, tree);
        self.handle.open_file(file.id).unwrap();
        status(om_server::FileStatus::Open);
    }

    /// The CA this host's certificate is verified against.
    pub fn ca_pem(&self) -> &[u8] {
        &self.ca_pem
    }

    /// A provider trusting this host and the host `ca` will be.
    pub fn provider_trusting_ca(&self, ca: &Ca) -> NativeProvider {
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        NativeProvider::new(NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.key_pair.serialize_pem().into_bytes(),
            additional_ca_pem: [self.ca_pem.as_slice(), ca.cert_pem.as_slice()].concat(),
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        })
    }

    /// A provider trusting this host, not connected yet.
    pub fn provider(&self) -> NativeProvider {
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        NativeProvider::new(NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.key_pair.serialize_pem().into_bytes(),
            additional_ca_pem: self.ca_pem.clone(),
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        })
    }

    /// The Admin account's connection to `file`.
    pub fn connection(&self, file: &str) -> Connection {
        Connection {
            host: "127.0.0.1".into(),
            port: self.port,
            file: file.into(),
            account: ACCOUNT.into(),
            password: PASSWORD.into(),
        }
    }

    /// `file` opened with a new provider.
    pub fn open(&self, file: &str) -> NativeProvider {
        let mut provider = self.provider();
        provider.open(&self.connection(file)).unwrap();
        provider
    }
}

/// Lets the commit saves [`Host::block_commit_saves`] failed work again,
/// including after a failed test.
pub struct CommitBarrier<'a>(&'a ServerHandle, u32);

impl Drop for CommitBarrier<'_> {
    fn drop(&mut self) {
        self.0.failing_commit_saves(self.1, false);
    }
}

/// Releases only the synthetic save barrier, including after a failed test.
pub struct SaveBarrier(PathBuf);

impl Drop for SaveBarrier {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// The GIOP messages of a recorded stream, in order.
fn giop_messages(mut rest: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (_, length) = om_giop::parse_header(rest).ok()?;
        let (message, after) = rest.split_at_checked(length)?;
        rest = after;
        Some(message)
    })
}

/// A host's certificate and key, made before the host serves
/// ([`Host::serving_at`]).
#[derive(Clone)]
pub struct Ca {
    pub cert_pem: Vec<u8>,
    key_pem: Vec<u8>,
}

impl Ca {
    pub fn new() -> Self {
        let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        Self {
            cert_pem: generated.cert.pem().into_bytes(),
            key_pem: generated.key_pair.serialize_pem().into_bytes(),
        }
    }
}

/// A server's recordings of its connections, removed when dropped.
pub struct Captures(pub PathBuf);

/// What one recorded request was: the file its connection had open, and
/// the RCTransaction.
pub type Transaction = (String, Vec<u8>);

impl Captures {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "om-related-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    /// Every `Perform` transaction the clients sent, with the file it
    /// names ("" before an Open): an Open by its file name, any other by its
    /// header's file id, which the Open replies gave out. Connection by
    /// connection, in the order the server numbered them (`conn-000`,
    /// `conn-001`, ...), each in the order it sent them. `read_dir` alone
    /// lists the folders in the file system's own order, which tmpfs and
    /// ext4 differ in. A connection may carry several files (#740).
    pub fn transactions(&self) -> Vec<Transaction> {
        let mut connections: Vec<PathBuf> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        connections.sort_by_key(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let index = name
                .strip_prefix("conn-")
                .and_then(|digits| digits.parse::<usize>().ok());
            (index.unwrap_or(usize::MAX), name)
        });
        let mut transactions = Vec::new();
        for connection in connections {
            let Ok(bytes) = std::fs::read(connection.join("c2s.bin")) else {
                continue;
            };
            // The file id each accepted Open's reply gave, by request id.
            let replies = std::fs::read(connection.join("s2c.bin")).unwrap_or_default();
            let opened: BTreeMap<u32, u32> = giop_messages(&replies)
                .filter_map(|message| {
                    let reply = om_giop::Reply::parse(message).ok()?;
                    let perform = om_protocol::parse_perform_reply(message).ok()?;
                    let open = om_protocol::parse_open_reply(&perform.result).ok()?;
                    Some((reply.request_id, open.file_id))
                })
                .collect();
            let mut names: BTreeMap<u32, String> = BTreeMap::new();
            let mut last = String::new();
            for message in giop_messages(&bytes) {
                let Ok(request) = om_giop::Request::parse(message) else {
                    continue;
                };
                if request.operation != b"Perform" {
                    continue;
                }
                let Some(perform) = om_protocol::parse_perform_request(request.body) else {
                    continue;
                };
                let transaction = perform.transaction;
                let file = if transaction.first() == Some(&(om_protocol::Opcode::OpenFile as u8)) {
                    let name = om_protocol::parse_open_request(transaction)
                        .unwrap()
                        .filename;
                    if let Some(id) = opened.get(&request.request_id) {
                        names.insert(*id, name.clone());
                    }
                    last = name.clone();
                    name
                } else {
                    // The header's `fileid` (network-protocol.md 4.1).
                    transaction
                        .get(12..16)
                        .and_then(|id| names.get(&u32::from_be_bytes(id.try_into().ok()?)))
                        .cloned()
                        .unwrap_or_else(|| last.clone())
                };
                transactions.push((file, transaction.to_vec()));
            }
        }
        transactions
    }

    /// The records of each `Tickle` the clients sent, connection by
    /// connection (network-protocol.md 6.1).
    pub fn tickles(&self) -> Vec<Vec<om_protocol::TickleSession>> {
        let Ok(connections) = std::fs::read_dir(&self.0) else {
            return Vec::new();
        };
        let mut tickles = Vec::new();
        for connection in connections {
            let Ok(bytes) = std::fs::read(connection.unwrap().path().join("c2s.bin")) else {
                continue;
            };
            for message in giop_messages(&bytes) {
                let Ok(request) = om_giop::Request::parse(message) else {
                    continue;
                };
                if request.operation != b"Tickle" {
                    continue;
                }
                let records = om_protocol::parse_tickle_request(request.body)
                    .and_then(|tickle| om_protocol::parse_tickle_parms(tickle.parms));
                tickles.extend(records);
            }
        }
        tickles
    }

    /// The transactions with opcode `op`.
    pub fn with_opcode(&self, op: om_protocol::Opcode) -> Vec<Transaction> {
        self.transactions()
            .into_iter()
            .filter(|(_, transaction)| transaction.first() == Some(&(op as u8)))
            .collect()
    }

    /// How many connections the clients made to the server.
    pub fn connections(&self) -> usize {
        std::fs::read_dir(&self.0).unwrap().count()
    }

    /// How many Open and Login requests each file got, and the host each
    /// Open named.
    pub fn opens_and_logins(&self) -> (BTreeMap<String, usize>, BTreeMap<String, usize>) {
        let mut opens = BTreeMap::new();
        let mut logins = BTreeMap::new();
        for (file, transaction) in self.transactions() {
            match transaction.first().copied() {
                Some(op) if op == om_protocol::Opcode::OpenFile as u8 => {
                    let open = om_protocol::parse_open_request(&transaction).unwrap();
                    assert_eq!(open.host, "127.0.0.1", "an Open names the host opened");
                    *opens.entry(file).or_default() += 1;
                }
                Some(op) if op == om_protocol::Opcode::Login as u8 => {
                    *logins.entry(file).or_default() += 1;
                }
                _ => {}
            }
        }
        (opens, logins)
    }

    /// The record downloads (op92 over `[T, 05]`): file, table id as sent
    /// and record count.
    pub fn row_downloads(&self) -> Vec<(String, u64, usize)> {
        let mut rows: Vec<_> = self
            .with_opcode(om_protocol::Opcode::DownloadList)
            .into_iter()
            .filter_map(|(file, transaction)| {
                let download = om_protocol::parse_download_list_request(&transaction).unwrap();
                let [table, records] = download.key_path.as_slice() else {
                    return None;
                };
                (records == &[5]).then(|| {
                    let keys = om_protocol::key_list_keys(&download.payload).unwrap();
                    (file, om_hbam::slot::be_uint(table).unwrap(), keys.len())
                })
            })
            .collect();
        rows.sort();
        rows
    }

    /// The first key of every download (op90) and download list (op92)
    /// path: the table a table's download names.
    pub fn downloaded_tables(&self) -> Vec<(String, u64)> {
        let mut tables = Vec::new();
        for (file, transaction) in self.transactions() {
            let path = match transaction.first().copied() {
                Some(op) if op == om_protocol::Opcode::DownloadList as u8 => {
                    om_protocol::parse_download_list_request(&transaction)
                        .unwrap()
                        .key_path
                }
                Some(op) if op == om_protocol::Opcode::DownloadWithLock as u8 => {
                    om_protocol::parse_download_request(&transaction)
                        .unwrap()
                        .key_path
                }
                _ => continue,
            };
            if let Some(first) = path.first() {
                tables.push((file, om_hbam::slot::be_uint(first).unwrap_or(0)));
            }
        }
        tables
    }
}

impl Drop for Captures {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
