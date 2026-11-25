//! A private host for one local file, embedded in the client that opened
//! it: how OpenMaker Client opens a `.fmp12` file itself, with the same
//! store, writes, locks, finds and saves OpenMaker Server uses
//! (`docs/re/local-files.md`).
//!
//! [`LocalHost::open`] reads the file through a locked descriptor
//! ([`crate::filelock`]: exclusive when the file can be written, shared
//! when it is read-only), and only once that descriptor is still the file
//! at its path: one opened just before another OpenMaker saved over the
//! file is dropped and the open tried again. It serves the file on an
//! ephemeral port of the loopback interface, with a certificate made for
//! this host alone, to the one client certificate it was given: no other
//! process can complete a TLS handshake with it. Logins are checked as
//! OpenMaker Server checks them, against the accounts the file stores
//! (`om_crypto::account`): only an active account of the file itself with
//! no password change due, in a privilege set the host enforces, may log
//! in, its kind and status read as FileMaker reads them
//! (`hosting::Eligibility`, [`crate::accounts`], docs/re/local-files.md
//! 2). A file FileMaker Server would not host (an empty [Full Access]
//! password, as the starter solutions have) opens, as in FileMaker Pro;
//! logins are not limited, and there is no license and no settings.
//!
//! A file is served read-only ([`ReadOnly`]) when it cannot be written
//! safely: this user or its folder may not write it, it has other names
//! (hard links, which a save's new copy would leave behind), it cannot be
//! locked here, or an entry is where a save would make its new copy. Its
//! writes are refused and nothing reaches the disk. A file that is not a
//! FileMaker Pro 12 file, or does not decode (damaged or encrypted at
//! rest), is refused before anything is written. Deferred field work
//! finished on opening stays in memory until an admitted account logs in.
//!
//! The host belongs to its client: [`LocalHost::close`] (or dropping it)
//! stops it, closes every connection it accepted (one stalled in its TLS
//! handshake included), waits for its threads and says whether nothing
//! holds the file any more, its lock and port released. Its client's
//! session is therefore never closed for silence. It serves at most
//! [`LOCAL_CONNECTIONS`] connections, and the server's other
//! [`ConnectionLimits`] still apply, against whatever else on this
//! computer connects to the port.

use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{info, warn};
use om_hbam::HbamFile;
use rustls::pki_types::CertificateDer;

use crate::config::{ConnectionLimits, HostedFile, ServerConfig, ServerIdentity};
use crate::connection::Shared;
#[cfg(target_os = "linux")]
use crate::engine::{export, ExportError, Progress};
use crate::engine::{
    ChangePassword, Credentials, FileEngine, Operation, Password, Reply, SaveFoundSet,
};
use crate::filelock::{link_count, same_file, FileLock, LockKind};
use crate::hosting::Hosting;
use crate::server::{Server, ServerHandle};
use crate::store::save_copy_path;
use crate::tls::{private_server_config, Identity};

/// How long [`LocalHost::close`] lets running scripts end, and then the
/// host's threads let go of the file.
const GRACE: Duration = Duration::from_secs(5);

/// Connections a local host serves at once: its one client holds a few
/// (the file's own session, and one for each other open file that reads
/// it as a related file), and the host shares the desktop process's
/// descriptors.
const LOCAL_CONNECTIONS: usize = 128;

/// `[23, 1, 1, 16]`: the account name in the account catalog's header.
const SUGGESTED_ACCOUNT: [&[u8]; 4] = [&[23], &[1], &[1], &[16]];

/// File Options' "Log in using" password (`KRequests`, local-files.md 3.1):
/// text, masked, not hashed.
const STORED_PASSWORD: [&[u8]; 4] = [&[23], &[1], &[1], &[6]];

/// How many times an open tries again when the file at the path was
/// replaced between its open and its lock (another OpenMaker saving it).
const OPEN_ATTEMPTS: u32 = 8;

/// Why a local file is served read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOnly {
    /// This user may not write the file, or its file system is read-only.
    NotWritable,
    /// The folder holding it does not allow the new copy a save writes.
    FolderNotWritable,
    /// The file has other names (hard links), which a save would leave
    /// with the old contents.
    OtherNames,
    /// It cannot be locked here (this platform or file system), so other
    /// programs' writes could not be kept out.
    NoLocks,
    /// Something is where a save would make its new copy
    /// (`<file>.omsave`, left by an earlier save?).
    CopyInTheWay,
}

impl std::fmt::Display for ReadOnly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ReadOnly::NotWritable => "you may not change it",
            ReadOnly::FolderNotWritable => "its folder does not allow saving it",
            ReadOnly::OtherNames => "it has other names (hard links) a save would not update",
            ReadOnly::NoLocks => "it cannot be locked here, so other programs could change it",
            ReadOnly::CopyInTheWay => {
                "an earlier save's copy (.omsave) is beside it; remove that to save changes"
            }
        })
    }
}

/// The file a host serves, locked when it can be.
enum Locked {
    Lock(FileLock),
    /// No locks here: the file is read-only ([`ReadOnly::NoLocks`]).
    Unlocked(File),
}

impl Locked {
    fn file(&self) -> &File {
        match self {
            Locked::Lock(lock) => lock.file(),
            Locked::Unlocked(file) => file,
        }
    }
}

/// Why a local file could not be opened. Nothing was written to it.
#[derive(Debug)]
pub enum LocalHostError {
    /// No file at the path.
    Missing(PathBuf),
    /// The path names a folder or something else that is not a file.
    NotAFile(PathBuf),
    /// The file cannot be read (permissions).
    Unreadable(io::Error),
    /// Another OpenMaker window or process has the file open.
    InUse(PathBuf),
    /// Not a FileMaker Pro 12 (`.fmp12`) file.
    NotFileMaker,
    /// A FileMaker file that does not decode: damaged, or encrypted at
    /// rest, which OpenMaker does not open.
    Undecodable(String),
    /// The host could not start (TLS identity, loopback port).
    Start(String),
}

impl LocalHostError {
    /// FileMaker's error code for it: 100 file missing, 802 unable to open
    /// file, 300 file in use, 807 not a FileMaker Pro file, 805 damaged.
    pub fn code(&self) -> i32 {
        match self {
            LocalHostError::Missing(_) => 100,
            LocalHostError::NotAFile(_)
            | LocalHostError::Unreadable(_)
            | LocalHostError::Start(_) => 802,
            LocalHostError::InUse(_) => 300,
            LocalHostError::NotFileMaker => 807,
            LocalHostError::Undecodable(_) => 805,
        }
    }
}

impl std::fmt::Display for LocalHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalHostError::Missing(path) => write!(f, "{} does not exist", path.display()),
            LocalHostError::NotAFile(path) => write!(f, "{} is not a file", path.display()),
            LocalHostError::Unreadable(e) => write!(f, "the file cannot be read: {e}"),
            LocalHostError::InUse(path) => write!(
                f,
                "{} is already open in OpenMaker or another application",
                path.display()
            ),
            LocalHostError::NotFileMaker => f.write_str("it is not a FileMaker Pro 12 file"),
            LocalHostError::Undecodable(e) => write!(
                f,
                "the file is damaged or encrypted (OpenMaker does not open encrypted files): {e}"
            ),
            LocalHostError::Start(e) => write!(f, "OpenMaker could not start the file: {e}"),
        }
    }
}

impl std::error::Error for LocalHostError {}

/// One local file, hosted privately for the client that opened it.
pub struct LocalHost {
    handle: Option<ServerHandle>,
    serve: Option<JoinHandle<()>>,
    port: u16,
    certificate_pem: Vec<u8>,
    name: String,
    path: PathBuf,
    read_only: Option<ReadOnly>,
}

impl std::fmt::Debug for LocalHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalHost")
            .field("path", &self.path)
            .field("port", &self.port)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

impl LocalHost {
    /// Open the file at `path` for the client whose certificate is
    /// `client_certificate_pem`, and serve it to that client alone.
    pub fn open(path: &Path, client_certificate_pem: &[u8]) -> Result<Self, LocalHostError> {
        let client = rustls_pemfile::certs(&mut &client_certificate_pem[..])
            .next()
            .and_then(Result::ok)
            .ok_or_else(|| LocalHostError::Start("no client certificate".into()))?;
        // The file's own path, through any links: two names for one file
        // are one file, and its lock is taken on it.
        let path = std::fs::canonicalize(path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => LocalHostError::Missing(path.to_path_buf()),
            _ => LocalHostError::Unreadable(e),
        })?;
        if !path.is_file() {
            return Err(LocalHostError::NotAFile(path));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| LocalHostError::NotAFile(path.clone()))?;
        let (locked, read_only) = open_locked(&path, &mut |_| {})?;
        let mut bytes = Vec::new();
        let mut reader = locked.file();
        reader
            .rewind()
            .and_then(|()| reader.read_to_end(&mut bytes))
            .map_err(LocalHostError::Unreadable)?;
        let hbam = HbamFile::from_bytes(bytes).map_err(|e| match e {
            om_hbam::Error::NotHbam => LocalHostError::NotFileMaker,
            other => LocalHostError::Undecodable(other.to_string()),
        })?;
        let hosted = HostedFile {
            name: name.clone(),
            path: path.clone(),
        };
        let lock = match locked {
            Locked::Lock(lock) => Some(lock),
            Locked::Unlocked(_) => None,
        };
        let engine = FileEngine::default();
        let hosting = Hosting::local(&hosted, &hbam, lock, read_only.is_some(), &engine)
            .map_err(|e| LocalHostError::Undecodable(e.to_string()))?;
        drop(hbam);

        let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])
            .map_err(|e| LocalHostError::Start(e.to_string()))?;
        let certificate_pem = generated.cert.pem().into_bytes();
        let identity = Identity::from_pem(
            &certificate_pem,
            generated.signing_key.serialize_pem().as_bytes(),
        )
        .map_err(|e| LocalHostError::Start(e.to_string()))?;
        let tls = private_server_config(identity, CertificateDer::from(client.to_vec()))
            .map_err(|e| LocalHostError::Start(e.to_string()))?;
        let config = ServerConfig {
            identity: ServerIdentity {
                name: "OpenMaker".into(),
                version_string: format!("OpenMaker Client {}", env!("CARGO_PKG_VERSION")),
                server_id: String::new(),
                web_service_settings: String::new(),
            },
            files: vec![hosted],
            insecure_files: true,
            record_dir: None,
        };
        let server = Server::start(
            "127.0.0.1:0",
            config,
            hosting,
            tls,
            true,
            Default::default(),
            engine,
        )
        .map_err(|e| LocalHostError::Start(e.to_string()))?
        .with_connection_limits(ConnectionLimits {
            connections: LOCAL_CONNECTIONS,
            per_address: LOCAL_CONNECTIONS,
            idle: None,
            ..ConnectionLimits::DEFAULT
        });
        let port = server
            .local_addr()
            .map_err(|e| LocalHostError::Start(e.to_string()))?
            .port();
        let handle = server
            .handle()
            .map_err(|e| LocalHostError::Start(e.to_string()))?;
        let serve = std::thread::Builder::new()
            .name("openmaker-local-file".into())
            .spawn(move || {
                if let Err(e) = server.serve() {
                    warn!("local file host: {e}");
                }
            })
            .map_err(|e| LocalHostError::Start(e.to_string()))?;
        match read_only {
            Some(reason) => info!("opened {} read-only: {reason}", path.display()),
            None => info!("opened {} for writing", path.display()),
        }
        Ok(Self {
            handle: Some(handle),
            serve: Some(serve),
            port,
            certificate_pem,
            name,
            path,
            read_only,
        })
    }

    /// The loopback port the file is served on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The host's certificate, the only one its client should trust.
    pub fn certificate_pem(&self) -> &[u8] {
        &self.certificate_pem
    }

    /// The name the client opens the file by: its file name.
    pub fn file_name(&self) -> &str {
        &self.name
    }

    /// The file's own path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the file is served read-only.
    pub fn is_read_only(&self) -> bool {
        self.read_only.is_some()
    }

    /// Why the file is served read-only, if it is.
    pub fn read_only(&self) -> Option<ReadOnly> {
        self.read_only
    }

    /// The account the file names in its account catalog's header (`[23,
    /// 1, 1, 16]`, read before Login like the rest of `[23, 1, 1]`), when
    /// it is one of the file's accounts: the name a client suggests when it
    /// asks for an account. Possibly FileMaker Pro's File Options account
    /// [I]; it is no evidence that FileMaker would log in without asking.
    pub fn suggested_account(&self) -> Option<String> {
        let handle = self.handle.as_ref()?;
        let hosted = handle.shared.hosting.files().into_iter().next()?;
        let name = {
            let store = hosted.read_store();
            let bytes = store.node(&SUGGESTED_ACCOUNT)?.assembled()?;
            om_scsu::decode_scsu(&bytes).ok()?
        };
        hosted.account(&name).map(|account| account.name.clone())
    }

    /// File Options' "Log in using" account and password (local-files.md
    /// 3.1, `[23, 1, 1, 16]` and `06`), with which FileMaker Pro opens the
    /// file without asking: `None` when the option is off or names the
    /// [Guest] account (its name is not text), which OpenMaker does not
    /// log in as (#158). The password is as the file stores it, in the
    /// clear once unmasked.
    pub fn stored_login(&self) -> Option<(String, zeroize::Zeroizing<String>)> {
        let handle = self.handle.as_ref()?;
        let hosted = handle.shared.hosting.files().into_iter().next()?;
        let store = hosted.read_store();
        let name = store.node(&SUGGESTED_ACCOUNT)?.assembled()?;
        // [Guest]'s login is its own name, `[` U+E002 `]`, for both.
        let account = om_scsu::decode_scsu(&name)
            .ok()
            .filter(|name| !name.is_empty() && name != "[\u{e002}]")?;
        // The password, wiped when dropped, its masked bytes too.
        let password = match store.node(&STORED_PASSWORD) {
            None => zeroize::Zeroizing::new(String::new()),
            // A plain value is the store's own bytes; a value assembled
            // from parts is a copy, wiped.
            Some(node) => match node.assembled()? {
                std::borrow::Cow::Borrowed(masked) => om_scsu::decode_scsu_secret(masked).ok()?,
                std::borrow::Cow::Owned(masked) => {
                    let masked = zeroize::Zeroizing::new(masked);
                    om_scsu::decode_scsu_secret(&masked).ok()?
                }
            },
        };
        Some((account, password))
    }

    /// Resolve a successful login's account key in this file only.
    pub fn authenticated_account(&self, result_key: &[u8]) -> om_data::host::AccountInfo {
        let Some(key) = om_data::host::security::account_key(result_key) else {
            return Default::default();
        };
        self.handle
            .as_ref()
            .and_then(|handle| handle.shared.hosting.files().into_iter().next())
            .and_then(|file| {
                file.accounts()
                    .iter()
                    .find(|account| account.key == key)
                    .map(|account| account.authenticated_info())
            })
            .unwrap_or_default()
    }

    /// Set account `name`'s password from `old` to `new`, as FileMaker
    /// sets the signed-in account's own ([`Operation::ChangePassword`]),
    /// its status stamped with this computer's local time [I: whether
    /// FileMaker stamps local time or UTC is unmeasured, local-files.md
    /// 2.4]. A read-only file changes nothing.
    pub fn change_password(
        &self,
        name: &str,
        old: &str,
        new: &str,
    ) -> Result<(), crate::hosting::PasswordChangeError> {
        let Some((shared, file)) = self.served() else {
            return Err(crate::hosting::PasswordChangeError::Refused);
        };
        if file.is_read_only() {
            return Err(crate::hosting::PasswordChangeError::Unwritten(
                "the file is read-only".into(),
            ));
        }
        let change = ChangePassword {
            account: crate::hosting::AccountRef::Name(name.to_owned()),
            manager: false,
            stamp: None,
        };
        let passwords = Credentials::Change {
            old: Password::new(old),
            new: Password::new(new),
        };
        match shared
            .engine
            .submit_with(&file, Operation::ChangePassword(change), passwords)
            .reply
        {
            Reply::ChangePassword(changed) => changed,
            other => Err(crate::hosting::PasswordChangeError::Unwritten(format!(
                "not a password change's reply: {other:?}"
            ))),
        }
    }

    /// The host's shared state and its one file, while it serves it.
    fn served(&self) -> Option<(&Shared, Arc<crate::hosting::Hosted>)> {
        let shared = &self.handle.as_ref()?.shared;
        let file = shared.hosting.files().into_iter().next()?;
        Some((shared, file))
    }

    /// Keep, at table occurrence `occurrence`'s `[TO, 07]`, the found set
    /// and record a window of this file closes on with table `table`
    /// (file-metadata.md 11), as FileMaker Pro keeps them when it closes a
    /// local file: `records` (`None`: every record) and `current`, at the
    /// table's data version. Whether it was written: a read-only file, a
    /// table without a data version, a node FileMaker Pro never wrote that
    /// way ([`om_data::meta::saved_found_set_written`]) and a save that
    /// fails (taken back) write nothing, and only the log says so.
    pub fn save_found_set(
        &self,
        occurrence: u64,
        table: u64,
        records: Option<om_hbam::RidList>,
        current: Option<u64>,
    ) -> bool {
        let Some((shared, file)) = self.served() else {
            return false;
        };
        let key = |value: u64| {
            let bytes = value.to_be_bytes();
            bytes[bytes.iter().position(|b| *b != 0).unwrap_or(7)..].to_vec()
        };
        let save = SaveFoundSet {
            occurrence: key(occurrence),
            table: key(table),
            records,
            current,
        };
        // A close never waits on another request's hold of the store.
        let Some(outcome) = shared
            .engine
            .try_submit(&file, Operation::SaveFoundSet(save))
        else {
            info!(
                "{}: the file is busy; the window's found set is not kept",
                self.name
            );
            return false;
        };
        outcome.reply == Reply::SaveFoundSet(true)
    }

    /// Save a Copy as (local-files.md 4, "Save a Copy as"): write the file
    /// as `kind` to `to`, a new file, as an export of the version the host
    /// holds (`crate::engine::export`, written and checked under the store's
    /// read lock, published after it). A copy of the current file is that
    /// version's export: the file's bytes as its last save left them, or as
    /// it was loaded when no save wrote it, and while changes a failed save
    /// left wait in memory, the tree written as the next save would write
    /// it. A copy that would lack such changes (a file hosted read-only for
    /// content the writer cannot write) is refused. A compacted copy is the
    /// tree written from the change number the next save starts from, as a
    /// compacted copy always was. Clones and self-contained copies are
    /// refused with [`io::ErrorKind::Unsupported`].
    ///
    /// The destination is validated before reading the store. See
    /// [`Self::prepare_copy`] for its platform and pathname requirements.
    pub fn save_copy(&self, to: &Path, kind: om_data::CopyKind) -> io::Result<()> {
        self.prepare_copy(to, kind)?()
    }

    /// Prepare a copy to an existing, protected Linux directory using an
    /// unnamed file. The returned operation snapshots the store when called
    /// so a script can validate the destination before committing a record.
    /// Dropping the operation has no visible filesystem side effects.
    #[cfg(target_os = "linux")]
    pub fn prepare_copy(
        &self,
        to: &Path,
        kind: om_data::CopyKind,
    ) -> io::Result<impl FnOnce() -> io::Result<()> + use<>> {
        use om_data::CopyKind;
        let compacted = match kind {
            CopyKind::Copy => false,
            CopyKind::Compacted => true,
            CopyKind::Clone | CopyKind::SelfContained => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "OpenMaker does not save clones or self-contained copies yet",
                ));
            }
        };
        let Some((engine, file)) = self.handle.as_ref().and_then(|handle| {
            let file = handle.shared.hosting.files().into_iter().next()?;
            Some((Arc::clone(&handle.shared.engine), file))
        }) else {
            return Err(io::Error::other("the file is closed"));
        };
        let mut destination = copy_destination::Destination::open(to)?;
        Ok(move || {
            let checked = {
                let reading = engine.read(&file);
                let source = if compacted {
                    reading.compacted_source()
                } else {
                    reading.export_source()?
                };
                reading.serve_once(|read| {
                    export::write_checked(read, source, &mut destination.file, Progress::none())
                })
            };
            match checked {
                Ok(checked) if checked.current => destination.publish(),
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the copy would lack changes that could not be saved",
                )),
                Err(ExportError::Io(e) | ExportError::Image(e)) => Err(e),
                Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
            }
        })
    }

    /// Copy destinations are currently supported on Linux only.
    #[cfg(not(target_os = "linux"))]
    pub fn prepare_copy(
        &self,
        _to: &Path,
        _kind: om_data::CopyKind,
    ) -> io::Result<impl FnOnce() -> io::Result<()> + use<>> {
        Err::<fn() -> io::Result<()>, _>(io::Error::new(
            io::ErrorKind::Unsupported,
            "OpenMaker saves copies of local files only on Linux",
        ))
    }

    /// Stop serving the file and wait until nothing holds it: its lock and
    /// port are released when this returns `true`, and `false` says that
    /// something (a script that did not stop in time) still does. The
    /// client should have closed its session first; every connection still
    /// open, in its TLS handshake or not, is closed.
    pub fn close(mut self) -> bool {
        self.shut_down()
    }

    fn shut_down(&mut self) -> bool {
        let Some(handle) = self.handle.take() else {
            return true;
        };
        for client in handle.clients() {
            handle.disconnect_client(client.id);
        }
        let scripts_ended = handle.shutdown(GRACE);
        handle.close_connections();
        if let Some(serve) = self.serve.take() {
            let _ = serve.join();
        }
        // Accepted before the accept loop stopped: closed too.
        handle.close_connections();
        // The host's state is dropped here, once the connection and
        // schedule threads have let go of it: dropping it closes the file's
        // descriptor, which releases the lock. A count of 0 read here while
        // another thread drops the last reference would come before that
        // thread has finished dropping it, the lock still held.
        let shared: Arc<Shared> = handle.into_shared();
        let deadline = Instant::now() + GRACE;
        while Arc::strong_count(&shared) > 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Nothing else can take a new reference (no weak one exists), so
        // this one is the last unless a thread outlived the grace.
        let released = match Arc::into_inner(shared) {
            Some(state) => {
                drop(state);
                true
            }
            None => false,
        };
        if !released {
            warn!("{}: still in use after closing", self.path.display());
        }
        info!("closed {}", self.path.display());
        scripts_ended && released
    }
}

impl Drop for LocalHost {
    fn drop(&mut self) {
        self.shut_down();
    }
}

#[cfg(target_os = "linux")]
mod copy_destination;

/// Open and lock the file at `path` as it is now, and say whether (and why)
/// it is to be read-only. The lock counts only while the descriptor it is
/// on is still the file at `path` (another OpenMaker's save may have put a
/// new file there since the open, leaving the old one's lock free): when
/// it is not, the open is tried again, and the file is in use when it keeps
/// changing. `before_lock` runs between each open and its lock (tests).
fn open_locked(
    path: &Path,
    before_lock: &mut dyn FnMut(u32),
) -> Result<(Locked, Option<ReadOnly>), LocalHostError> {
    let in_use = || LocalHostError::InUse(path.to_path_buf());
    for attempt in 0..OPEN_ATTEMPTS {
        let (file, mut read_only) = open_file(path)?;
        if read_only.is_none() && std::fs::symlink_metadata(save_copy_path(path)).is_ok() {
            read_only = Some(ReadOnly::CopyInTheWay);
        }
        if read_only.is_none() && matches!(link_count(&file), Ok(n) if n > 1) {
            read_only = Some(ReadOnly::OtherNames);
        }
        before_lock(attempt);
        let kind = if read_only.is_some() {
            LockKind::Shared
        } else {
            LockKind::Exclusive
        };
        let locked = match FileLock::acquire(file, kind) {
            Ok(lock) => Locked::Lock(lock),
            Err((e, _)) if e.kind() == io::ErrorKind::WouldBlock => return Err(in_use()),
            Err((e, file)) if e.kind() == io::ErrorKind::Unsupported => {
                read_only = read_only.or(Some(ReadOnly::NoLocks));
                Locked::Unlocked(file)
            }
            Err((e, _)) => return Err(LocalHostError::Unreadable(e)),
        };
        match same_file(locked.file(), path) {
            Ok(true) => {}
            // Replaced (or removed) since the open: the lock guards nothing.
            Ok(false) => {
                info!("{} changed while opening; opening again", path.display());
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            // No file identity here (Windows): the file has no lock either.
            Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                read_only = read_only.or(Some(ReadOnly::NoLocks));
            }
            Err(e) => return Err(LocalHostError::Unreadable(e)),
        }
        // A name added between the check and the lock: open again shared.
        if read_only.is_none() && !matches!(link_count(locked.file()), Ok(1)) {
            continue;
        }
        return Ok((locked, read_only));
    }
    Err(in_use())
}

/// Open `path` for writing when this user may write it and the folder that
/// holds it (saves write a new copy beside it), else for reading, and why
/// it is read-only.
fn open_file(path: &Path) -> Result<(File, Option<ReadOnly>), LocalHostError> {
    let missing = |e: io::Error| match e.kind() {
        io::ErrorKind::NotFound => LocalHostError::Missing(path.to_path_buf()),
        _ => LocalHostError::Unreadable(e),
    };
    match File::options().read(true).write(true).open(path) {
        Ok(file) => Ok((
            file,
            (!folder_writable(path)).then_some(ReadOnly::FolderNotWritable),
        )),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            File::open(path)
                .map(|file| (file, Some(ReadOnly::NotWritable)))
                .map_err(missing)
        }
        Err(e) => Err(missing(e)),
    }
}

#[cfg(unix)]
fn folder_writable(path: &Path) -> bool {
    path.parent()
        .is_some_and(|dir| rustix::fs::access(dir, rustix::fs::Access::WRITE_OK).is_ok())
}

#[cfg(not(unix))]
fn folder_writable(path: &Path) -> bool {
    path.parent()
        .and_then(|dir| std::fs::metadata(dir).ok())
        .is_some_and(|m| !m.permissions().readonly())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn folder(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("om-local-open-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Another OpenMaker's save, as `Store::save` makes it: a new file
    /// renamed over `path`, locked by that OpenMaker when `locked`.
    fn saved_over(path: &Path, bytes: &[u8], locked: bool) -> Option<FileLock> {
        let new = path.with_extension("new");
        std::fs::write(&new, bytes).unwrap();
        let file = File::options().read(true).write(true).open(&new).unwrap();
        let lock = locked.then(|| FileLock::acquire(file, LockKind::Exclusive).unwrap());
        std::fs::rename(&new, path).unwrap();
        lock
    }

    fn contents(locked: &Locked) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut file = locked.file();
        file.rewind().unwrap();
        file.read_to_end(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn a_descriptor_saved_over_between_its_open_and_its_lock_is_not_kept() {
        let dir = folder("stale");
        let path = dir.join("file.fmp12");
        std::fs::write(&path, b"initial").unwrap();
        // The other OpenMaker still holds its new file: this open finds the
        // stale descriptor's lock free, sees the path has moved on, opens
        // again and finds the file in use, rather than keeping the old one.
        let mut other = None;
        let refused = open_locked(&path, &mut |attempt| {
            if attempt == 0 {
                other = saved_over(&path, b"committed", true);
            }
        });
        assert!(matches!(refused, Err(LocalHostError::InUse(_))));
        assert!(other.is_some());
        drop(other);
        // Saved over and closed meanwhile: the open ends on the new file.
        let (locked, read_only) = open_locked(&path, &mut |attempt| {
            if attempt == 0 {
                saved_over(&path, b"committed again", false);
            }
        })
        .unwrap();
        assert_eq!(read_only, None);
        assert!(same_file(locked.file(), &path).unwrap());
        assert_eq!(contents(&locked), b"committed again");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_that_keeps_changing_is_in_use() {
        let dir = folder("changing");
        let path = dir.join("file.fmp12");
        std::fs::write(&path, b"initial").unwrap();
        let refused = open_locked(&path, &mut |_| {
            saved_over(&path, b"again", false);
        });
        assert!(matches!(refused, Err(LocalHostError::InUse(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_name_added_before_the_lock_makes_the_open_read_only() {
        let dir = folder("named");
        let path = dir.join("file.fmp12");
        std::fs::write(&path, b"initial").unwrap();
        let (locked, read_only) = open_locked(&path, &mut |attempt| {
            if attempt == 0 {
                std::fs::hard_link(&path, dir.join("second name")).unwrap();
            }
        })
        .unwrap();
        assert_eq!(read_only, Some(ReadOnly::OtherNames));
        assert!(matches!(&locked, Locked::Lock(lock) if lock.kind() == LockKind::Shared));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
