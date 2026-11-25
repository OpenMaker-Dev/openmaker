//! What an administrator sees of a running server and does with it: the
//! server itself, its hosted files, the connected clients with the files
//! each has open, sending a client a message, disconnecting it, and
//! blocking new users (`docs/admin-console.md`, "Administration in
//! om-server"; FileMaker Server's views of them in
//! `docs/re/admin-console.md` sections 4.2, 7, 8 and 9). OpenMaker's Admin
//! Console and Admin API are to be built on these.
//!
//! Each connection keeps its client's entry in [`Clients`] current: at
//! Connect, Open, Login, Logout and the end of its session. Snapshots take
//! only the registry's lock and read the files' metadata on disk; they never
//! wait for a busy file's store. What the administrator asks of a client
//! goes to its connection through their shared [`Control`]; only the
//! connection's own thread writes to the client's socket.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use log::{error, info, warn};
use om_protocol::notify::AdminMessage;
use om_protocol::write::Notification;
use om_protocol::MachineInfo;

use crate::connection::Shared;
use crate::eventlog::{ClientLabel, Event};
use crate::hosting::{FileClose, ReopenError};
use crate::scripts::AbortReason;
use crate::server::ServerHandle;
#[cfg(debug_assertions)]
use crate::store::SaveFault;

/// The server, as the administrator sees it (the Admin API's
/// `/server/metadata`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerOverview {
    /// The server's name ([`crate::ServerIdentity::name`]).
    pub name: String,
    /// The version string clients see.
    pub version: String,
    /// The server's 32-hex id.
    pub server_id: String,
    /// When the server started ([`crate::Server::bind`]).
    pub started_at: SystemTime,
    /// The address it listens on for FileMaker clients (RCFMP12).
    pub address: SocketAddr,
    /// How many files it hosts, open or not (the Admin API's
    /// `totalDBCount`).
    pub hosted_files: usize,
    /// How many of them are open, paused or closing: served to the clients
    /// that have them open (the Admin API's `openDBCount`).
    pub open_files: usize,
}

/// What state a hosted file is in: FileMaker Server's states
/// (`docs/re/admin-console.md` 7). Every hosted file is opened when the
/// server starts; the administrator closes, opens, pauses and resumes it
/// ([`ServerHandle::close_file`] and the others).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    /// Open and served to clients (`NORMAL`).
    Open,
    /// Its clients were asked to close it; no one may open it (`CLOSING`).
    Closing,
    /// Not served: Open (op78) is refused as for a file the server does
    /// not host (`CLOSED`).
    Closed,
    /// Being loaded from disk to be opened (`OPENING`).
    Opening,
    /// Served, but every request on it waits unanswered until it is
    /// resumed (`PAUSED`).
    Paused,
}

impl FileStatus {
    /// FileMaker Server's name for the state.
    pub fn filemaker_name(self) -> &'static str {
        match self {
            FileStatus::Open => "NORMAL",
            FileStatus::Closing => "CLOSING",
            FileStatus::Closed => "CLOSED",
            FileStatus::Opening => "OPENING",
            FileStatus::Paused => "PAUSED",
        }
    }
}

/// A hosted file, as the administrator sees it (the Admin API's
/// `databases[]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileInfo {
    /// The server's id for the file (the Open reply's `file_id`).
    pub id: u32,
    /// The name clients open it by.
    pub name: String,
    /// Where it lives on disk, made absolute.
    pub path: PathBuf,
    /// The folder that holds it.
    pub folder: PathBuf,
    /// Its size on disk in bytes, or `None` when its metadata cannot be read.
    pub size: Option<u64>,
    pub status: FileStatus,
    /// How many connected clients have it open.
    pub clients: usize,
    /// Why it is closed, when it failed to open again
    /// ([`ServerHandle::open_file`]); or, while it is served, why it takes
    /// no more changes: a save renamed its new copy over the file and could
    /// not flush the folder, so whether the file on disk keeps that change
    /// is unknown (docs/server-storage.md 5). Closing it and opening it
    /// again, or a restart, clears that.
    pub error: Option<String>,
}

/// Why a [`ServerHandle`] file operation ([`ServerHandle::close_file`],
/// [`ServerHandle::open_file`], [`ServerHandle::pause_file`],
/// [`ServerHandle::resume_file`]) did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileError {
    /// No hosted file has that id (the Admin API answers 404 with
    /// FileMaker code 1700).
    UnknownFile,
    /// The file's state does not allow it (the Admin API answers 409 with
    /// FileMaker code 1709): for example opening a file that is not closed.
    NotAllowed(FileStatus),
    /// The text is longer than [`MAX_MESSAGE_CHARS`] characters.
    TextTooLong,
    /// The grace time is longer than [`MAX_GRACE`].
    GraceTooLong,
    /// The file failed to load from disk, for this reason; it stays
    /// closed.
    Load(String),
    /// The server could not start the thread that finishes a close; the
    /// file stays open.
    NoThread,
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::UnknownFile => f.write_str("no hosted file has that id"),
            FileError::NotAllowed(status) => write!(
                f,
                "the operation is not allowed while the file is {}",
                status.filemaker_name()
            ),
            FileError::TextTooLong => f.write_str("the message is longer than 200 characters"),
            FileError::GraceTooLong => f.write_str("the grace time is longer than 3600 seconds"),
            FileError::Load(reason) => write!(f, "the file failed to open: {reason}"),
            FileError::NoThread => f.write_str("the server could not start the close"),
        }
    }
}

impl std::error::Error for FileError {}

/// A connected client, as the administrator sees it (the Admin API's
/// `clients[]`): a connection with a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectedClient {
    /// The connection id `Connect` handed out, for
    /// [`ServerHandle::disconnect_client`].
    pub id: u32,
    /// The client's address.
    pub address: SocketAddr,
    /// The client's machine name (`machineName` of Connect's machine info,
    /// network-protocol.md 5.2).
    pub machine_name: String,
    /// The client's application type (`appType` of Connect's machine info):
    /// `0x01` for FileMaker Pro; calc-functions.md 6 lists FileMaker's
    /// application types.
    pub app_type: u8,
    /// The client's version string, e.g. `"Pro 26.0.3"`.
    pub version: String,
    /// The session name the client gave in its latest accepted Login, which
    /// FileMaker Server shows as "Session Identifier"; `None` before one.
    pub session_name: Option<String>,
    /// When the session started (its `Connect`).
    pub connected_at: SystemTime,
    /// The files it has open, in id order.
    pub files: Vec<ClientFile>,
    /// The administrator disconnected it: at once, its connection closing
    /// ([`ServerHandle::disconnect_client`]), or after its grace time
    /// ([`ServerHandle::disconnect_client_with_notice`]).
    pub disconnecting: bool,
}

/// The most characters a message to a client has: the Admin API's limit
/// for `messageText`.
pub const MAX_MESSAGE_CHARS: usize = 200;

/// How many of the administrator's messages may wait for one client.
/// A client that answers the server's LocateRequest for its callback
/// object takes each within a read poll (about 100 ms); more wait only for
/// one that does not answer it (OpenMaker's choice).
pub const MAX_WAITING_MESSAGES: usize = 16;

/// Why [`ServerHandle::send_message`] sent nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageError {
    /// No connected client has that id (the Admin API answers 404 with
    /// FileMaker code 1700).
    UnknownClient,
    /// The text is empty or only white space.
    EmptyText,
    /// The text is longer than [`MAX_MESSAGE_CHARS`] characters.
    TextTooLong,
    /// The client has no file open, and a message names one of its files.
    NoOpenFile,
    /// The client is being disconnected: its connection is closing.
    Disconnecting,
    /// [`MAX_WAITING_MESSAGES`] messages already wait for the client, which
    /// has not answered the server's LocateRequest for its callback object.
    Busy,
    /// The server's name is too long to go in a message (over 32,000
    /// characters).
    ServerNameTooLong,
}

impl std::fmt::Display for MessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MessageError::UnknownClient => "no connected client has that id",
            MessageError::EmptyText => "the message is empty",
            MessageError::TextTooLong => "the message is longer than 200 characters",
            MessageError::NoOpenFile => "the client has no file open",
            MessageError::Disconnecting => "the client is being disconnected",
            MessageError::Busy => "the client is not taking messages",
            MessageError::ServerNameTooLong => "the server's name is too long for a message",
        })
    }
}

impl std::error::Error for MessageError {}

/// The grace time a client gets to close its files when it is
/// disconnected with notice, unless the administrator gives another: the
/// Admin API's default for `graceTime`.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(90);

/// The longest grace time: the Admin API's limit for `graceTime`.
pub const MAX_GRACE: Duration = Duration::from_secs(3600);

/// What the administrator tells a client before it must close its files
/// and leave ([`ServerHandle::disconnect_client_with_notice`]): the Admin
/// API's `messageText` and `graceTime`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The administrator's text: 0 to [`MAX_MESSAGE_CHARS`] characters.
    pub message: String,
    /// How long the client has to close its files: at most [`MAX_GRACE`].
    pub grace: Duration,
}

impl Default for Notice {
    /// No text and [`DEFAULT_GRACE`].
    fn default() -> Self {
        Self {
            message: String::new(),
            grace: DEFAULT_GRACE,
        }
    }
}

/// Why [`ServerHandle::disconnect_client_with_notice`] did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisconnectError {
    /// No connected client has that id (the Admin API answers 404 with
    /// FileMaker code 1700).
    UnknownClient,
    /// The text is longer than [`MAX_MESSAGE_CHARS`] characters.
    TextTooLong,
    /// The grace time is longer than [`MAX_GRACE`].
    GraceTooLong,
    /// The client is already being disconnected.
    Disconnecting,
    /// [`MAX_WAITING_MESSAGES`] messages already wait for the client, which
    /// has not answered the server's LocateRequest for its callback object.
    Busy,
    /// The server's name is too long to go in a message (over 32,000
    /// characters).
    ServerNameTooLong,
}

impl std::fmt::Display for DisconnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DisconnectError::UnknownClient => "no connected client has that id",
            DisconnectError::TextTooLong => "the message is longer than 200 characters",
            DisconnectError::GraceTooLong => "the grace time is longer than 3600 seconds",
            DisconnectError::Disconnecting => "the client is already being disconnected",
            DisconnectError::Busy => "the client is not taking messages",
            DisconnectError::ServerNameTooLong => "the server's name is too long for a message",
        })
    }
}

impl std::error::Error for DisconnectError {}

/// A disconnect with notice under way: what the client's connection needs
/// to finish it (`Connection::farewell`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Farewell {
    /// The administrator's text, repeated in the forced notice.
    pub text: String,
    /// When the client's grace time ends.
    pub grace_ends: Instant,
    /// Once the grace time has ended: when the client's time to log out
    /// and disconnect ends ([`crate::ConnectionLimits::logout_wait`]).
    pub leave_by: Option<Instant>,
}

/// What the administrator asks of one connection, shared by its client's
/// entry in [`Clients`], its [`ClientSocket`] and the connection itself,
/// whose thread does it: nothing else writes to the client's socket.
#[derive(Debug, Default)]
pub(crate) struct Control {
    /// End the session and close the connection now
    /// ([`ServerHandle::disconnect_client`]).
    disconnect: AtomicBool,
    /// The administrator's messages waiting to go to the session's client
    /// in `Notify` calls, oldest first; at most [`MAX_WAITING_MESSAGES`].
    notices: Mutex<VecDeque<Notification>>,
    /// The session's disconnect with notice, while it is under way.
    farewell: Mutex<Option<Farewell>>,
}

impl Control {
    /// Whether the administrator disconnected the client at once.
    pub(crate) fn disconnecting(&self) -> bool {
        self.disconnect.load(Ordering::Acquire)
    }

    /// Disconnect the client at once: its connection's next read fails.
    pub(crate) fn disconnect_now(&self) {
        self.disconnect.store(true, Ordering::Release);
    }

    fn farewell_slot(&self) -> MutexGuard<'_, Option<Farewell>> {
        self.farewell.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The session's disconnect with notice, if one is under way.
    pub(crate) fn farewell(&self) -> Option<Farewell> {
        self.farewell_slot().clone()
    }

    /// Replace the disconnect with notice under way (`None`: it ended with
    /// the session).
    pub(crate) fn set_farewell(&self, farewell: Option<Farewell>) {
        *self.farewell_slot() = farewell;
    }

    /// The waiting messages, also after a thread panicked holding them
    /// (each change is one push or drain).
    fn notices(&self) -> MutexGuard<'_, VecDeque<Notification>> {
        self.notices.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether messages wait for the client.
    pub(crate) fn has_notices(&self) -> bool {
        !self.notices().is_empty()
    }

    /// The messages waiting for the client, oldest first.
    pub(crate) fn take_notices(&self) -> Vec<Notification> {
        self.notices().drain(..).collect()
    }

    /// Drop the waiting messages, returning how many there were: the
    /// session they were for ended, or its client has no callback object.
    pub(crate) fn clear_notices(&self) -> usize {
        let mut notices = self.notices();
        let dropped = notices.len();
        notices.clear();
        dropped
    }

    /// Queue a message the connection itself sends (the forced notice).
    pub(crate) fn queue_notice(&self, notice: Notification) -> Result<(), MessageError> {
        self.queue(notice)
    }

    fn queue(&self, notice: Notification) -> Result<(), MessageError> {
        let mut notices = self.notices();
        if notices.len() >= MAX_WAITING_MESSAGES {
            return Err(MessageError::Busy);
        }
        notices.push_back(notice);
        Ok(())
    }
}

/// A file a client has open (the Admin API's `guestFiles[]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientFile {
    /// The file's id ([`FileInfo::id`]).
    pub id: u32,
    /// The name clients open it by.
    pub name: String,
    /// The account logged in to it: `None` before an accepted Login and
    /// after Logout.
    pub account: Option<String>,
}

/// The connected clients: an entry for each connection with a session, by
/// connection id. The sessions Perform Script on Server runs scripts in are
/// not clients ([`ServerHandle::scripts`] lists those scripts).
#[derive(Debug, Default)]
pub(crate) struct Clients {
    entries: Mutex<BTreeMap<u32, Entry>>,
}

/// One client's entry.
#[derive(Debug)]
struct Entry {
    address: SocketAddr,
    machine_name: String,
    app_type: u8,
    version: String,
    session_name: Option<String>,
    connected_at: SystemTime,
    files: BTreeMap<u32, ClientFile>,
    /// How `Access.log` names the client: set at its first accepted Login,
    /// the same for every line of the session.
    access: Option<ClientLabel>,
    /// Shared with the connection, which does what it asks.
    control: Arc<Control>,
}

impl Clients {
    /// The entries, also after a thread panicked holding them (each change
    /// leaves them whole).
    fn entries(&self) -> MutexGuard<'_, BTreeMap<u32, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Connection `id` from `address` started a session (Connect) for the
    /// client `machine` describes; the administrator asks things of it
    /// through `control`.
    pub(crate) fn connected(
        &self,
        id: u32,
        address: SocketAddr,
        machine: &MachineInfo,
        control: Arc<Control>,
    ) {
        self.entries().insert(
            id,
            Entry {
                address,
                machine_name: machine.machine_name.clone(),
                app_type: machine.app_type,
                version: machine.version_string.clone(),
                session_name: None,
                connected_at: SystemTime::now(),
                files: BTreeMap::new(),
                access: None,
                control,
            },
        );
    }

    /// Connection `id` opened file `file` (Open), not logged in to it yet.
    pub(crate) fn opened(&self, id: u32, file: u32, name: &str) {
        if let Some(entry) = self.entries().get_mut(&id) {
            entry.files.insert(
                file,
                ClientFile {
                    id: file,
                    name: name.to_owned(),
                    account: None,
                },
            );
        }
    }

    /// `account` logged in to file `file` on connection `id` (Login), whose
    /// client calls its session `session_name`.
    pub(crate) fn logged_in(&self, id: u32, file: u32, account: &str, session_name: &str) {
        if let Some(entry) = self.entries().get_mut(&id) {
            if let Some(file) = entry.files.get_mut(&file) {
                file.account = Some(account.to_owned());
            }
            entry.session_name = Some(session_name.to_owned());
        }
    }

    /// The account logged in to file `file` on connection `id` logged out
    /// (Logout); the file stays open.
    pub(crate) fn logged_out(&self, id: u32, file: u32) {
        if let Some(file) = self
            .entries()
            .get_mut(&id)
            .and_then(|entry| entry.files.get_mut(&file))
        {
            file.account = None;
        }
    }

    /// Connection `id` closed file `file` (an op62 Unlock of its root).
    pub(crate) fn closed(&self, id: u32, file: u32) {
        if let Some(entry) = self.entries().get_mut(&id) {
            entry.files.remove(&file);
        }
    }

    /// Connection `id`'s session ended.
    pub(crate) fn ended(&self, id: u32) {
        self.entries().remove(&id);
    }

    /// Every client, oldest session first.
    fn snapshot(&self) -> Vec<ConnectedClient> {
        let mut clients: Vec<ConnectedClient> = self
            .entries()
            .iter()
            // A client that closed its files when asked to disconnect is
            // not listed while its session ends, as FileMaker Server lists
            // it no more (re 9).
            .filter(|(_, entry)| !entry.files.is_empty() || entry.control.farewell_slot().is_none())
            .map(|(id, entry)| ConnectedClient {
                id: *id,
                address: entry.address,
                machine_name: entry.machine_name.clone(),
                app_type: entry.app_type,
                version: entry.version.clone(),
                session_name: entry.session_name.clone(),
                connected_at: entry.connected_at,
                files: entry.files.values().cloned().collect(),
                disconnecting: entry.control.disconnecting()
                    || entry.control.farewell_slot().is_some(),
            })
            .collect();
        clients.sort_by_key(|client| (client.connected_at, client.id));
        clients
    }

    /// How many clients have each file open, by file id.
    fn open_counts(&self) -> HashMap<u32, usize> {
        let mut counts = HashMap::new();
        for file in self.entries().values().flat_map(|entry| entry.files.keys()) {
            *counts.entry(*file).or_default() += 1;
        }
        counts
    }

    /// Connection `id`'s client is named `label` in `Access.log` from now
    /// on (its first accepted Login).
    pub(crate) fn labelled(&self, id: u32, label: &ClientLabel) {
        if let Some(entry) = self.entries().get_mut(&id) {
            entry.access = Some(label.clone());
        }
    }

    /// How `Access.log` names client `id`: as all its session's lines do,
    /// or by its machine's name before an accepted Login.
    fn label(&self, id: u32) -> Option<ClientLabel> {
        let entries = self.entries();
        let entry = entries.get(&id)?;
        Some(entry.access.clone().unwrap_or_else(|| {
            let machine = &entry.machine_name;
            ClientLabel::new(machine, machine, entry.address.ip())
        }))
    }

    /// Ask connection `id` to end its session; its address, or `None` when
    /// it has no session.
    fn disconnect(&self, id: u32) -> Option<SocketAddr> {
        let entries = self.entries();
        let entry = entries.get(&id)?;
        entry.control.disconnect.store(true, Ordering::Release);
        Some(entry.address)
    }

    /// Start disconnecting client `id` with `notice`: queue the request to
    /// close its files that `build` makes, given the host's id of one it
    /// has open, and hand the rest to its connection; the client's address.
    fn farewell(
        &self,
        id: u32,
        notice: &Notice,
        build: impl FnOnce(u32) -> Result<Notification, DisconnectError>,
    ) -> Result<SocketAddr, DisconnectError> {
        let entries = self.entries();
        let entry = entries.get(&id).ok_or(DisconnectError::UnknownClient)?;
        let control = &entry.control;
        if control.disconnecting() || control.farewell_slot().is_some() {
            return Err(DisconnectError::Disconnecting);
        }
        let now = Instant::now();
        let farewell = |grace_ends, leave_by| Farewell {
            text: notice.message.clone(),
            grace_ends,
            leave_by,
        };
        match entry.files.keys().next() {
            // Nothing to close: the session ends at once, as when a client
            // has closed its files (OpenMaker's choice).
            None => control.set_farewell(Some(farewell(now, Some(now)))),
            // No time to close: the forced notice goes out at once
            // (OpenMaker's choice; FileMaker Server's is unmeasured).
            Some(_) if notice.grace.is_zero() => control.set_farewell(Some(farewell(now, None))),
            Some(file) => {
                control
                    .queue(build(*file)?)
                    .map_err(|_| DisconnectError::Busy)?;
                control.set_farewell(Some(farewell(now + notice.grace, None)));
            }
        }
        Ok(entry.address)
    }

    /// Queue the message `build` makes, given the host's id of a file
    /// client `id` has open, for the client's connection to send; the
    /// client's address. Under the registry's lock, so a session that has
    /// ended takes no message: its connection drops what waits after it
    /// removed the entry.
    fn message(
        &self,
        id: u32,
        build: impl FnOnce(u32) -> Result<Notification, MessageError>,
    ) -> Result<SocketAddr, MessageError> {
        let entries = self.entries();
        let entry = entries.get(&id).ok_or(MessageError::UnknownClient)?;
        if entry.control.disconnecting() {
            return Err(MessageError::Disconnecting);
        }
        // A client with several files open gets one message, naming the
        // file with the lowest id (OpenMaker's choice: FileMaker Pro's
        // dialog names the server, not the file).
        let file = *entry.files.keys().next().ok_or(MessageError::NoOpenFile)?;
        entry.control.queue(build(file)?)?;
        Ok(entry.address)
    }
}

/// A client's socket as its connection reads and writes it. Once the
/// administrator disconnects the client, every read fails, so the
/// connection stops within one read however the client goes on sending:
/// between requests, inside a fragmented message or inside a TLS record.
/// Writes still go out, the reply to a request in progress and the TLS
/// close_notify included.
pub(crate) struct ClientSocket {
    tcp: TcpStream,
    /// Shared with the connection and the client's entry in [`Clients`].
    control: Arc<Control>,
}

impl ClientSocket {
    pub(crate) fn new(tcp: TcpStream, control: Arc<Control>) -> Self {
        Self { tcp, control }
    }
}

impl Read for ClientSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.control.disconnecting() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "disconnected by the administrator",
            ));
        }
        self.tcp.read(buf)
    }
}

impl Write for ClientSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tcp.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

/// For one catalog: the last member id and modification count
/// GenerateMemberKey issued that wait to be written, and those the tree
/// holds ([`ServerHandle::member_keys`]).
#[cfg(debug_assertions)]
#[doc(hidden)]
pub type MemberKeys = (Option<(u64, u64)>, (u64, u64));

impl ServerHandle {
    /// The server: its identity, when it started, where it listens and how
    /// many files it hosts.
    pub fn overview(&self) -> ServerOverview {
        let identity = &self.shared.identity;
        ServerOverview {
            name: identity.name.clone(),
            version: identity.version_string.clone(),
            server_id: identity.server_id.clone(),
            started_at: self.shared.started,
            address: self.addr,
            hosted_files: self.shared.hosting.slots().len(),
            open_files: self.shared.hosting.files().len(),
        }
    }

    /// Make every commit's save of hosted file `id` fail (`fail`), as
    /// storage refusing it would, or work again, while its other saves (an
    /// issued record id's, #927) work: for tests of a commit that cannot be
    /// saved after its record ids were. Whether the file is open. Only in
    /// debug builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn failing_commit_saves(&self, id: u32, fail: bool) -> bool {
        let Some(hosted) = self
            .shared
            .hosting
            .slots()
            .iter()
            .find(|slot| slot.id == id)
            .and_then(|slot| slot.served())
        else {
            return false;
        };
        hosted.fail_commit_saves(fail);
        true
    }

    /// Make every save of hosted file `id` fail at `fault` (before the
    /// rename, or flushing the folder after it), or work again with `None`:
    /// for tests of an operation whose change is not stored, or whose
    /// outcome is unknown. Whether the file is served. Only in debug
    /// builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn failing_saves(&self, id: u32, fault: Option<SaveFault>) -> bool {
        let Some(hosted) = self
            .shared
            .hosting
            .slots()
            .iter()
            .find(|slot| slot.id == id)
            .and_then(|slot| slot.served())
        else {
            return false;
        };
        hosted.fail_saves(fault);
        true
    }

    /// For catalog `catalog` of hosted file `id`: the last member id and
    /// modification count GenerateMemberKey issued that wait to be
    /// written, and those its tree holds, from which the next key is one
    /// more. `None` when the file is not served. Only in debug builds, for
    /// tests that check a refused request reserved no key.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn member_keys(&self, id: u32, catalog: &[Vec<u8>]) -> Option<MemberKeys> {
        let hosted = self
            .shared
            .hosting
            .slots()
            .iter()
            .find(|slot| slot.id == id)
            .and_then(|slot| slot.served())?;
        let keys = hosted.read_store().member_keys_for_tests(catalog);
        Some(keys)
    }

    /// The hosted files in id order, open or not, each with its state and
    /// how many connected clients have it open. Sizes come from the file
    /// system, so a file busy with a request is not waited for.
    pub fn files(&self) -> Vec<FileInfo> {
        let counts = self.shared.clients.open_counts();
        self.shared
            .hosting
            .slots()
            .iter()
            .map(|slot| {
                let path = std::path::absolute(&slot.path).unwrap_or_else(|_| slot.path.clone());
                FileInfo {
                    id: slot.id,
                    name: slot.name.clone(),
                    folder: path.parent().map(Path::to_path_buf).unwrap_or_default(),
                    size: std::fs::metadata(&path).ok().map(|metadata| metadata.len()),
                    path,
                    status: slot.status(),
                    clients: counts.get(&slot.id).copied().unwrap_or(0),
                    error: slot.error().or_else(|| {
                        slot.served()
                            .and_then(|file| file.writes_refused().map(str::to_owned))
                    }),
                }
            })
            .collect()
    }

    /// Close hosted file `id` ([`FileInfo::id`]) the way FileMaker Server
    /// does for the Admin API's `PATCH /databases/{id}` with `CLOSED`
    /// (`docs/re/admin-console.md` 9, network-protocol.md 6.5): it becomes
    /// [`FileStatus::Closing`] and no one may open it; each client that
    /// has it open is asked to close it (code `0x0040` with
    /// `notice.message`), again halfway through `notice.grace` (FileMaker
    /// Server's second request came 45 s into its default 90), and sent the
    /// forced notice (`0x0870`) when the grace time ends; one that still
    /// has it open `ConnectionLimits::logout_wait` later is disconnected
    /// at once. When the grace time ends the file's server scripts and
    /// schedule runs are aborted, and once nothing holds the file (no
    /// session, script, schedule run or request) it is
    /// [`FileStatus::Closed`]: no longer served, its old store gone, so
    /// nothing of it can save over the file opened again. New sessions,
    /// scripts and schedule runs are refused meanwhile. Only an open file is
    /// closed. Returns at once; the clients' connections and a thread of
    /// the close take the steps.
    pub fn close_file(&self, id: u32, notice: &Notice) -> Result<(), FileError> {
        if notice.message.chars().count() > MAX_MESSAGE_CHARS {
            return Err(FileError::TextTooLong);
        }
        if notice.grace > MAX_GRACE {
            return Err(FileError::GraceTooLong);
        }
        let slot = self.shared.hosting.slot(id).ok_or(FileError::UnknownFile)?;
        slot.start_close(FileClose {
            text: notice.message.clone(),
            started: Instant::now(),
            grace: notice.grace,
        })
        .map_err(FileError::NotAllowed)?;
        info!(
            "{:?}: closing for the administrator in {:?}",
            slot.name, notice.grace
        );
        let shared = self.shared.clone();
        let watcher = std::thread::Builder::new()
            .name(format!("close file {id}"))
            .spawn(move || finish_close(&shared, id));
        if let Err(e) = watcher {
            slot.cancel_close();
            error!(
                "{:?}: not closing: no thread to finish the close: {e}",
                slot.name
            );
            return Err(FileError::NoThread);
        }
        Ok(())
    }

    /// Open hosted file `id` again (the Admin API's `OPENED`): it is
    /// loaded from disk with the checks it had when the server started,
    /// since the administrator may have replaced it while it was closed,
    /// and served again; [`FileStatus::Opening`] while it loads. Only a
    /// closed file is opened. One that fails to load stays closed, with
    /// the reason in [`FileInfo::error`] and in the error returned.
    pub fn open_file(&self, id: u32) -> Result<(), FileError> {
        match self
            .shared
            .hosting
            .reopen(id, &self.shared.events, &self.shared.engine)
        {
            Ok(()) => {
                info!("file {id}: opened for the administrator");
                Ok(())
            }
            Err(ReopenError::UnknownFile) => Err(FileError::UnknownFile),
            Err(ReopenError::NotAllowed(status)) => Err(FileError::NotAllowed(status)),
            Err(ReopenError::Load(reason)) => {
                warn!("file {id}: failed to open for the administrator: {reason}");
                Err(FileError::Load(reason))
            }
        }
    }

    /// Pause hosted file `id` (the Admin API's `PAUSED`): from now on each
    /// client request on it waits, unanswered, until
    /// [`ServerHandle::resume_file`], while its connection goes on reading
    /// and answering everything else (Tickles, other files' requests), as
    /// FileMaker Server does (network-protocol.md 6.1, 6.5). A request
    /// being answered when it is paused finishes first. Clients are sent
    /// nothing. Only an open file is paused.
    pub fn pause_file(&self, id: u32) -> Result<(), FileError> {
        let slot = self.shared.hosting.slot(id).ok_or(FileError::UnknownFile)?;
        slot.change(FileStatus::Open, FileStatus::Paused)
            .map_err(FileError::NotAllowed)?;
        info!("{:?}: paused for the administrator", slot.name);
        Ok(())
    }

    /// Resume paused file `id` (the Admin API's `RESUMED`): the requests
    /// that waited are answered in the order they came, within each
    /// connection's next read poll (about 100 ms).
    pub fn resume_file(&self, id: u32) -> Result<(), FileError> {
        let slot = self.shared.hosting.slot(id).ok_or(FileError::UnknownFile)?;
        slot.change(FileStatus::Paused, FileStatus::Open)
            .map_err(FileError::NotAllowed)?;
        info!("{:?}: resumed for the administrator", slot.name);
        Ok(())
    }

    /// The connected clients, oldest session first, each with the files it
    /// has open and the account logged in to each.
    pub fn clients(&self) -> Vec<ConnectedClient> {
        self.shared.clients.snapshot()
    }

    /// Disconnect client `id` ([`ConnectedClient::id`]). Returns whether it
    /// was connected.
    ///
    /// The disconnect is immediate. The client's connection notices it at
    /// its next read of the socket, within one read poll (about 100 ms)
    /// however the client goes on sending ([`ClientSocket`]), and stops
    /// answering: a request it is answering is finished first (a Perform
    /// Script on Server that waits for its script included), one that
    /// arrives later is not. The
    /// session then ends as when the client leaves: its locks and its
    /// registrations for change notifications are released, the
    /// notifications and script callback results waiting for it are
    /// dropped, and its scripts that do not wait run on. The connection is
    /// then closed with a TLS close_notify. Until then the client is listed
    /// as [`ConnectedClient::disconnecting`].
    ///
    /// This disconnect sends the client nothing: it learns of it when its
    /// connection closes. [`ServerHandle::disconnect_client_with_notice`]
    /// first asks it to close its files, as FileMaker Server does. Called
    /// for a client already being disconnected with notice, it ends that at
    /// once.
    pub fn disconnect_client(&self, id: u32) -> bool {
        let label = self.shared.clients.label(id);
        let Some(address) = self.shared.clients.disconnect(id) else {
            return false;
        };
        info!("{address}: disconnecting session 0x{id:08x} for the administrator");
        if let Some(client) = label {
            self.shared
                .events
                .record(Event::client_disconnected(&client));
        }
        true
    }

    /// Disconnect client `id` ([`ConnectedClient::id`]) after asking it to
    /// close its files, as FileMaker Server does for the Admin API's
    /// `DELETE /clients/{id}` with `messageText` and `graceTime`
    /// (`docs/re/admin-console.md` 9, network-protocol.md 6.5). Returns at
    /// once; the client's connection does the rest.
    ///
    /// - The client is sent a request to close its files (code `0x0850`)
    ///   with `notice.message`, naming the open file with the lowest id.
    ///   FileMaker Pro shows its "Close File" dialog and closes the files
    ///   30 seconds later, whatever the grace time.
    /// - A client closes a file with an op62 Unlock of its root, which
    ///   releases what its session holds in the file. Once it has closed
    ///   them all it is no longer listed, and when the grace time ends and
    ///   [`crate::ConnectionLimits::logout_wait`] (30 s) more have passed,
    ///   the server calls `Disconnect` on it and ends its session, as
    ///   FileMaker Server did. Its connection stays open for a new
    ///   session.
    /// - A client that still has a file open when the grace time ends is
    ///   sent the forced notice (`0x0870`, the same text), on which
    ///   FileMaker Pro logs out and disconnects at once. One that has not
    ///   done so `logout_wait` later is disconnected at once, as
    ///   [`ServerHandle::disconnect_client`] does.
    ///
    /// A grace time of 0 sends the forced notice at once, and a client
    /// with no file open is disconnected (the `Disconnect` call) at once
    /// (both OpenMaker's choices). Until its session ends the client is
    /// listed as [`ConnectedClient::disconnecting`] (FileMaker Server's
    /// Admin API lists it as `NORMAL`).
    pub fn disconnect_client_with_notice(
        &self,
        id: u32,
        notice: &Notice,
    ) -> Result<(), DisconnectError> {
        if notice.message.chars().count() > MAX_MESSAGE_CHARS {
            return Err(DisconnectError::TextTooLong);
        }
        if notice.grace > MAX_GRACE {
            return Err(DisconnectError::GraceTooLong);
        }
        let server_name = &self.shared.identity.name;
        let address = self.shared.clients.farewell(id, notice, |file| {
            AdminMessage::close_all(file, &notice.message, server_name)
                .to_notification()
                .map_err(|_| DisconnectError::ServerNameTooLong)
        })?;
        info!(
            "{address}: disconnecting session 0x{id:08x} for the administrator in {:?}",
            notice.grace
        );
        Ok(())
    }

    /// Send client `id` ([`ConnectedClient::id`]) a message from the
    /// administrator (the Admin API's `POST /clients/{id}/message`).
    /// FileMaker Pro shows it as "Message from “<server name>”" and closes
    /// the dialog by itself after about 30 seconds
    /// (`docs/re/admin-console.md` 9).
    ///
    /// `text` has 1 to [`MAX_MESSAGE_CHARS`] characters and not only white
    /// space, as the Admin API requires. The message goes to the client in a
    /// `Notify` call on its own connection (network-protocol.md 6.5),
    /// naming one file the client has open by the server's id for it: the
    /// one with the lowest id when it has several, so it gets one message
    /// (OpenMaker's choice). It is queued here and sent by the client's
    /// connection within its next read poll (about 100 ms), after the
    /// server located the client's callback object once in the session.
    /// `Ok` means it was queued: the client answers each call, but nothing
    /// waits for that.
    pub fn send_message(&self, id: u32, text: &str) -> Result<(), MessageError> {
        if text.chars().all(char::is_whitespace) {
            return Err(MessageError::EmptyText);
        }
        if text.chars().count() > MAX_MESSAGE_CHARS {
            return Err(MessageError::TextTooLong);
        }
        let server_name = &self.shared.identity.name;
        let address = self.shared.clients.message(id, |file| {
            AdminMessage::message(file, text, server_name)
                .to_notification()
                .map_err(|_| MessageError::ServerNameTooLong)
        })?;
        info!("{address}: sending session 0x{id:08x} a message from the administrator");
        Ok(())
    }

    /// Whether Block New Users is on ([`ServerHandle::set_block_new_users`]).
    pub fn block_new_users(&self) -> bool {
        self.shared.block_new_users.load(Ordering::Acquire)
    }

    /// Turn Block New Users on or off (the Admin API's
    /// `/server/config/blocknewusers`). While it is on, a client's Open
    /// (op78) is refused with FileMaker's error 1638 ("The host is not
    /// allowing new connections. Try again later."), as FileMaker Server
    /// refuses it; `Connect` still succeeds. A session that already has a
    /// file open is not a new user and may open more (OpenMaker's choice:
    /// FileMaker Server's tooltip says current sessions are not affected,
    /// and opening another file is unmeasured). Clients already connected
    /// keep working, and nothing is sent to them.
    pub fn set_block_new_users(&self, block: bool) {
        let was = self.shared.block_new_users.swap(block, Ordering::AcqRel);
        if was != block {
            info!(
                "Block New Users {} by the administrator",
                if block { "turned on" } else { "turned off" }
            );
        }
    }
}

/// How often a close under way checks whether its file has drained.
const CLOSE_POLL: Duration = Duration::from_millis(50);

/// Finish the close of hosted file `id` ([`ServerHandle::close_file`]):
/// from the end of the grace time, abort the file's server scripts
/// (schedule runs included), looking again at every wait, since a script
/// admitted just before the close may start only after an earlier look;
/// close the file once nothing holds it any more
/// ([`crate::hosting::Slot::finish_close_if_drained`]). Clients that keep
/// it are forced out by their connections.
fn finish_close(shared: &Shared, id: u32) {
    let mut aborted = std::collections::HashSet::new();
    loop {
        let Some(slot) = shared.hosting.slot(id) else {
            return;
        };
        let Some(close) = slot.closing() else {
            return;
        };
        if close.started.elapsed() >= close.grace {
            let name = slot.name.to_lowercase();
            for script in shared.scripts.running() {
                if script.file.to_lowercase() == name && aborted.insert(script.id) {
                    shared
                        .scripts
                        .abort(AbortReason::FileClosed, |running, _| running == script.id);
                }
            }
        }
        if slot.finish_close_if_drained() {
            info!("{:?}: closed", slot.name);
            return;
        }
        std::thread::sleep(CLOSE_POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(port: u16) -> SocketAddr {
        SocketAddr::from(([192, 0, 2, 7], port))
    }

    fn machine(name: &str) -> MachineInfo {
        MachineInfo {
            machine_name: name.into(),
            version_string: "Pro 26.0.3".into(),
            app_type: 0x01,
            ..MachineInfo::default()
        }
    }

    #[test]
    fn follows_a_session_from_connect_to_its_end() {
        let clients = Clients::default();
        let before = SystemTime::now();
        let control = Arc::new(Control::default());
        clients.connected(7, address(50001), &machine("Front Desk"), control.clone());
        let [client] = clients.snapshot().try_into().unwrap();
        assert_eq!(
            (
                client.id,
                client.address,
                client.machine_name.as_str(),
                client.app_type,
                client.version.as_str(),
                client.session_name,
                client.files.len(),
                client.disconnecting,
            ),
            (
                7,
                address(50001),
                "Front Desk",
                0x01,
                "Pro 26.0.3",
                None,
                0,
                false
            )
        );
        assert!(before <= client.connected_at && client.connected_at <= SystemTime::now());

        clients.opened(7, 2, "Tasks.fmp12");
        clients.opened(7, 1, "Contacts.fmp12");
        clients.logged_in(7, 2, "Admin", "Front Desk session");
        let client = &clients.snapshot()[0];
        assert_eq!(client.session_name.as_deref(), Some("Front Desk session"));
        assert_eq!(
            client.files,
            [
                ClientFile {
                    id: 1,
                    name: "Contacts.fmp12".into(),
                    account: None,
                },
                ClientFile {
                    id: 2,
                    name: "Tasks.fmp12".into(),
                    account: Some("Admin".into()),
                },
            ]
        );

        // Logout leaves the file open; opening it again starts over.
        clients.logged_out(7, 2);
        assert_eq!(clients.snapshot()[0].files[1].account, None);
        clients.logged_in(7, 2, "Admin", "Front Desk session");
        clients.opened(7, 2, "Tasks.fmp12");
        assert_eq!(clients.snapshot()[0].files[1].account, None);

        clients.ended(7);
        assert!(clients.snapshot().is_empty());
        // A connection that has no session changes nothing.
        clients.opened(7, 2, "Tasks.fmp12");
        clients.logged_in(7, 2, "Admin", "Front Desk session");
        assert!(clients.snapshot().is_empty());
        assert!(!control.disconnecting());
    }

    #[test]
    fn lists_clients_oldest_first_and_counts_open_files() {
        let clients = Clients::default();
        for (id, port, second) in [(30, 50003, 1), (10, 50001, 2), (20, 50002, 2)] {
            clients.connected(id, address(port), &machine("m"), Default::default());
            clients.entries().get_mut(&id).unwrap().connected_at =
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(second);
        }
        let ports: Vec<u16> = clients
            .snapshot()
            .iter()
            .map(|client| client.address.port())
            .collect();
        // Sessions that started at the same time are in id order.
        assert_eq!(ports, [50003, 50001, 50002]);

        clients.opened(10, 1, "Contacts.fmp12");
        clients.opened(20, 1, "Contacts.fmp12");
        clients.opened(20, 2, "Tasks.fmp12");
        // Opening a file twice on one connection is one client.
        clients.opened(20, 2, "Tasks.fmp12");
        assert_eq!(clients.open_counts(), HashMap::from([(1, 2), (2, 1)]));
        clients.ended(20);
        assert_eq!(clients.open_counts(), HashMap::from([(1, 1)]));
    }

    #[test]
    fn a_disconnect_flags_the_connection_until_its_session_ends() {
        let clients = Clients::default();
        let control = Arc::new(Control::default());
        clients.connected(7, address(50001), &machine("Front Desk"), control.clone());
        assert_eq!(clients.disconnect(8), None);
        assert!(!control.disconnecting());

        assert_eq!(clients.disconnect(7), Some(address(50001)));
        assert!(control.disconnecting());
        assert!(clients.snapshot()[0].disconnecting);

        clients.ended(7);
        assert_eq!(clients.disconnect(7), None);
    }

    #[test]
    fn a_message_names_an_open_file_and_waits_for_the_connection() {
        let clients = Clients::default();
        let control = Arc::new(Control::default());
        clients.connected(7, address(50001), &machine("Front Desk"), control.clone());
        let notice = |file: u32| Notification {
            code: 0x0810,
            message: file.to_be_bytes().to_vec(),
        };
        assert_eq!(
            clients.message(8, |file| Ok(notice(file))),
            Err(MessageError::UnknownClient)
        );
        assert_eq!(
            clients.message(7, |file| Ok(notice(file))),
            Err(MessageError::NoOpenFile)
        );
        clients.opened(7, 5, "Tasks.fmp12");
        clients.opened(7, 2, "Contacts.fmp12");
        assert_eq!(
            clients.message(7, |file| Ok(notice(file))),
            Ok(address(50001))
        );
        assert_eq!(control.take_notices(), [notice(2)]);

        // A client that takes none is not sent more than the bound.
        for _ in 0..MAX_WAITING_MESSAGES {
            clients.message(7, |file| Ok(notice(file))).unwrap();
        }
        assert_eq!(
            clients.message(7, |file| Ok(notice(file))),
            Err(MessageError::Busy)
        );
        assert_eq!(control.clear_notices(), MAX_WAITING_MESSAGES);

        clients.disconnect(7);
        assert_eq!(
            clients.message(7, |file| Ok(notice(file))),
            Err(MessageError::Disconnecting)
        );
    }
}
