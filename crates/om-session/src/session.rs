//! An RCFMP12 session: connect, open a hosted file, log in, then issue
//! `Perform` operations.
//!
//! Handles GIOP fragment reassembly and the server's bidirectional back-channel:
//! between replies the server may call the client's callback object. A
//! `Perform` carrying op105 (the result of a Perform Script on Server with
//! Callback) is kept for [`Session::take_psos_results`], a `Notify` call's
//! change notifications for [`Session::take_notifications`] and the
//! administrator's messages in it for [`Session::take_admin_messages`];
//! other calls get an empty answer. The server's calls are read and
//! answered while the session reads: during its own exchanges (the
//! keepalive's Tickles included) and [`Session::poll`].
//!
//! While a file is open a keepalive thread sends `Tickle` when the
//! connection has been silent for [`KEEPALIVE_INTERVAL`], as FileMaker Pro
//! does (network-protocol.md 6.1), so the host does not close it as idle.
//! The thread and the session's requests share the connection behind one
//! lock held from a request to its reply, so a Tickle never goes out in the
//! middle of another exchange, and they draw request ids from the same
//! sequence. A failed transport, the host closing the connection, or a
//! Tickle unanswered for [`TICKLE_TIMEOUT`] marks the session lost
//! ([`Session::connection_lost`], which never waits for an exchange): later
//! requests fail at once with [`SessionError::ConnectionLost`] without
//! touching the socket.
//!
//! One connection carries several files, as FileMaker Pro opens a file's
//! related files in the session it already has (network-protocol.md 5.5):
//! [`Session::share`] gives another handle on the connection, and each
//! handle opens, logs in to and closes a file of its own. The connection's
//! keepalive and its transaction counters are shared. A change
//! notification goes to the handle whose op63 registered the client file
//! id it names, a Perform Script on Server result to the handle that sent
//! its callback id; what names neither goes to the file opened first of
//! those still open. A handle dropped while others go on closes its file (op62); the
//! connection ends with its last handle, and a lost connection loses every
//! file on it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use om_giop::{build_reply, parse_header, MsgType, Reply, ReplyStatus, Request};
use om_hbam::OwnedNode;
use om_protocol::notify::{self, AdminMessage};
use om_protocol::script::{self, CallbackIds, PerformScript, PerformScriptReply, PsosResult};
use om_protocol::write;
pub use om_protocol::KEEPALIVE_INTERVAL;
use om_protocol::{
    bidir_service_context, build_connect, build_download, build_download_list, build_is_a,
    build_is_a_of, build_login, build_open, build_perform, build_perform_reply,
    build_perform_with_max, build_query, build_tickle, hosted_files, op, parse_connect_reply,
    parse_download_list_reply, parse_download_reply, parse_login_reply, parse_open_reply,
    parse_perform_request, parse_query_reply, parse_tickle_reply, ConnectReply, DownloadListReply,
    DownloadListRequest, DownloadReply, DownloadRequest, LoginEnvironment, LoginReply,
    LoginRequest, MachineInfo, Opcode, OpenReply, PerformReply, QueryReply, QueryRequest, Resume,
    TickleSession, DOWNLOAD_LIST_MAX_RETURN, DOWNLOAD_MAX_RETURN, FILE_SESSION_ID, PAGE_LIMIT,
    QUERY_MAX_RETURN,
};
use om_rctransaction::ResultBuilder;

use crate::tls::{Transport, TransportError, READ_TIMEOUT};

/// Key of a table's records node, `[T, 05]` (write-path.md 5.2).
const RECORDS: u8 = 0x05;

thread_local! {
    /// [`record_requests_sent`] of this thread.
    static RECORD_REQUESTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many requests that may carry or commit record data this thread has
/// handed to any connection (#152), a count that only grows: two equal
/// readings around a save prove that it sent no host record data, on any
/// connection, for any file it wrote. A reading that grew tells nothing
/// of what a host kept.
///
/// A request counts once it goes to the transport of a connection not
/// known to be lost, before its bytes are written, so one that fails on
/// the way counts too. Every Perform counts but those of the
/// [`RECORD_FREE`] operations or a fully decoded authoring Describe.
/// Unknown or unreadable requests and Perform Script on Server count too. Connect, `_is_a` and Tickle carry no
/// record data.
///
/// The count is the thread's because a session's exchanges run on the
/// thread that asks for them, so a provider's writes, through every file
/// and connection a save reaches, run on its caller's. Threads it starts
/// for work of its own add their counts to the caller's before the caller
/// goes on ([`add_record_requests_sent`]). The keepalive thread sends only
/// Tickles.
pub fn record_requests_sent() -> u64 {
    RECORD_REQUESTS.with(std::cell::Cell::get)
}

/// Count on this thread `count` requests [`record_requests_sent`] counted
/// on a thread it waited for.
pub fn add_record_requests_sent(count: u64) {
    RECORD_REQUESTS.with(|sent| sent.set(sent.get().saturating_add(count)));
}

/// The operations whose Perform neither carries nor commits record data,
/// among those a save sends before its records (#152):
/// - reads: op15, op89, op90, op92, op93;
/// - locks: op60, op62, op65;
/// - op71, after which no record exists (write-path.md 3.1);
/// - op72, which advances a serial number (3.2);
/// - op82, the host's clock;
/// - op87/op88, library acquisition/release, tracked separately from records;
/// - opening a file: op78, op80, op63.
///
/// Every other operation counts. That includes op14, whatever its flags
/// say, and op86, whose program can delete records.
const RECORD_FREE: [Opcode; 16] = [
    Opcode::DownloadFromHost,
    Opcode::CompareModCounts,
    Opcode::DownloadWithLock,
    Opcode::DownloadList,
    Opcode::DownloadFromTemp,
    Opcode::Lock,
    Opcode::Unlock,
    Opcode::KillAllLocks,
    Opcode::HostCreateRecord,
    Opcode::HostGetSerial,
    Opcode::HostTimeStamp,
    Opcode::OpenFile,
    Opcode::Login,
    Opcode::RequestNotification,
    Opcode::AdjustReferenceCount,
    Opcode::UploadBinaryData,
];

/// What a request may carry ([`record_requests_sent`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carries {
    Records,
    NoRecords,
}

/// What a Perform of `transaction` may carry: no records only when its
/// operation is one of [`RECORD_FREE`] or an exact authoring capability read.
fn carries(transaction: &[u8]) -> Carries {
    match om_rctransaction::Header::parse(transaction) {
        Some(header) if RECORD_FREE.iter().any(|op| *op as u8 == header.opcode) => {
            Carries::NoRecords
        }
        Some(header) if header.opcode == Opcode::OpenMakerAuthoring as u8 => {
            // Only Describe is a pure capability read. Check its operation
            // before parsing so a mutation's definition is never allocated here;
            // the complete parse also rejects truncated or trailing parameters.
            use om_protocol::authoring::{self, Operation, Request};
            if matches!(
                authoring::request_operation(transaction),
                Ok(Operation::Describe)
            ) && matches!(authoring::parse_request(transaction), Ok(Request::Describe))
            {
                Carries::NoRecords
            } else {
                Carries::Records
            }
        }
        _ => Carries::Records,
    }
}

#[cfg(test)]
mod submission_tests {
    use super::{carries, Carries};
    use om_protocol::authoring::{build_request, Request};

    #[test]
    fn only_complete_authoring_describe_is_record_free() {
        for (request, expected) in [
            (Request::Describe, Carries::NoRecords),
            (Request::Save, Carries::Records),
            (Request::DescribeSecurity, Carries::Records),
            (
                Request::Unknown {
                    version: 255,
                    operation: 255,
                },
                Carries::Records,
            ),
        ] {
            let wire = build_request(1, 1, &request).unwrap();
            assert_eq!(carries(&wire), expected);
            if expected == Carries::NoRecords {
                assert_eq!(carries(&wire[..wire.len() - 1]), Carries::Records);
                let mut trailing = wire;
                trailing.push(0);
                assert_eq!(carries(&trailing), Carries::Records);
            }
        }
    }
}

/// A connected session to a FileMaker Server: a handle on a connection,
/// with at most one file open ([`Session::share`] for another).
pub struct Session {
    /// The connection, shared with the keepalive thread and the
    /// connection's other handles.
    link: Arc<Link>,
    connection_id: u32,
    identity: ConnectReply,
    /// This handle on the connection, as the inbox routes to it.
    key: u64,
    /// The file opened by [`Session::open_file`] on this handle, if any.
    opened: Option<OpenedFile>,
    /// The client file id this handle's op63 registered its file under
    /// ([`Session::request_notifications`]).
    client_file: Option<i32>,
    /// The status of the last Open the host refused on this handle
    /// ([`Session::refused_open`]): each handle opens its own file.
    refused_open: Option<u16>,
    /// The protocol version the server's Login reply named.
    protocol_version: Option<u16>,
}

/// Which connection a [`Session`] handle is on ([`Session::connection`]):
/// equal for the handles of one connection ([`Session::share`]), and
/// unequal for any other connection, to the same host or not, which has
/// its own transaction ids. It does not keep the connection open.
#[derive(Clone)]
pub struct ConnectionRef(Weak<Link>);

impl PartialEq for ConnectionRef {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ConnectionRef {}

impl std::fmt::Debug for ConnectionRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectionRef(..)")
    }
}

/// The connection and what the keepalive thread needs of it.
struct Link {
    wire: Mutex<Wire>,
    /// Wakes the keepalive thread: its interval changed or the session ends.
    wake: Condvar,
    /// The wire's [`Wire::lost`], readable while an exchange holds the wire.
    lost: Arc<OnceLock<String>>,
    /// The keepalive thread, from the time the first file opens.
    keepalive: Mutex<Option<JoinHandle<()>>>,
    /// Once this connection is lost and a file on it reconnected: the new
    /// connection, which the other files that were on this one reopen on
    /// ([`Session::successor`]).
    successor: Mutex<Option<Successor>>,
}

/// The connection a lost one's file reconnected on (#1011's files after a
/// loss): only its other files on the same host and port, admitted for the
/// same account and password, reopen there.
struct Successor {
    /// Weak: the lost connection does not keep the new one open.
    link: Weak<Link>,
    connection_id: u32,
    identity: ConnectReply,
    host: String,
    port: u16,
    admission: Admission,
}

impl Link {
    fn lock(&self) -> MutexGuard<'_, Wire> {
        // A panic while holding the lock leaves the wire usable: every
        // exchange either completed or marked the connection lost.
        self.wire.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// [`Self::lock`], unless an exchange holds the wire now.
    fn try_lock(&self) -> Option<MutexGuard<'_, Wire>> {
        match self.wire.try_lock() {
            Ok(wire) => Some(wire),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

/// The connection's state. Whoever holds it owns the socket for one whole
/// exchange.
struct Wire {
    /// `None` once the session is closed or its connection lost.
    transport: Option<Transport>,
    connection_id: u32,
    /// GIOP request ids: even, step 2, shared by every request.
    next_request_id: u32,
    /// What the server sent on its own between replies.
    inbox: Inbox,
    /// When the last exchange (a Tickle included) ended: the keepalive
    /// counts from here.
    last_request: Instant,
    /// When the last exchange other than a Tickle ended: the idle time a
    /// Tickle reports.
    last_used: Instant,
    /// How long the connection may stay silent; `None` sends no Tickle.
    keepalive: Option<Duration>,
    /// How long a Tickle waits for its reply, whatever else arrives.
    tickle_timeout: Duration,
    /// The header's `transidlo`: one counter for the connection, whichever
    /// file a transaction names (network-protocol.md 4.1).
    next_file_transaction: u32,
    /// Write transaction ids (locks, uploads, commits): one sequence for
    /// the connection, since a host keys a lock's holder by connection and
    /// transaction (OpenMaker Server's `LockUser`).
    next_transaction: u32,
    /// The key of the next handle made on the connection.
    next_handle: u64,
    /// Handles on the connection ([`Session::share`]): it ends with the last.
    handles: usize,
    /// The hosted files open (or being opened) on the connection, each
    /// with the handles holding it ([`Session::claim_file`], #1011).
    files: Vec<HeldFile>,
    /// The host's ids of the files the host disconnected (`0x0870`), which
    /// the connection has let go of ([`Wire::route_admin`]): what a handle
    /// asks of one fails as lost, and closing it sends nothing.
    revoked: BTreeSet<u32>,
    /// Why the connection was lost, once it is.
    lost: Arc<OnceLock<String>>,
    /// The session is ending: the keepalive thread stops.
    closing: bool,
}

/// Messages the server sent on its own, kept until the caller takes them,
/// and which file's handle takes each.
#[derive(Debug, Default)]
struct Inbox {
    psos_results: VecDeque<PsosResult>,
    /// Change notifications, each with the client file id its message
    /// names ([`notified_file`]) and the handles yet to take it: each
    /// handle holding that file takes its own copy (#1011).
    notifications: Vec<(Option<i32>, write::Notification, Vec<u64>)>,
    /// At most [`MAX_ADMIN_MESSAGES`], the latest, as they came.
    admin_messages: VecDeque<AdminMessage>,
    /// Those routed to the handles that take them ([`Wire::route_admin`]):
    /// a notice naming a file, with the handles holding that file yet to
    /// take their copy; one for the whole connection (Send Message, the
    /// request to close every file) with `None`, which the first handle
    /// asking takes. At most [`MAX_ADMIN_MESSAGES`].
    admin_routed: VecDeque<(AdminMessage, Option<Vec<u64>>)>,
    /// The handles with a file open, the first opened first, each with
    /// the client file id its op63 registered.
    open: Vec<(u64, Option<i32>)>,
    /// Each Perform Script on Server callback id sent on the connection:
    /// the handle that sent it and the caller's own id for it. The
    /// connection gives each call an id of its own, since each caller (each
    /// file's script host) counts its ids from 1.
    callbacks: BTreeMap<u32, (u64, u32)>,
    /// The id the connection gives the next callback.
    next_callback: u32,
    /// Client file ids of files closed on the connection: what still names
    /// one is dropped.
    retired: BTreeSet<i32>,
    /// Callback ids of calls whose file was closed: their results are
    /// dropped.
    retired_callbacks: BTreeSet<u32>,
}

/// How many of the administrator's messages a session keeps until they
/// are taken; older ones are dropped (OpenMaker's choice).
const MAX_ADMIN_MESSAGES: usize = 64;

/// Why a handle's file is lost when the host disconnected it (`0x0870`).
pub const HOST_DISCONNECTED_FILE: &str = "The host disconnected the file.";

/// Who a Login admitted a file for: an opaque digest of the account and
/// password, which the caller computes, never the credentials. A handle
/// holds a file another handle opened only for the same admission
/// ([`Session::claim_file`]). Prints nothing.
#[derive(Clone, PartialEq, Eq)]
pub struct Admission([u8; 32]);

impl Admission {
    /// The admission a digest of an account and its password stands for.
    pub fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}

impl std::fmt::Debug for Admission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Admission(..)")
    }
}

/// What [`Session::claim_file`] got this handle.
#[derive(Debug)]
pub enum Claim {
    /// The file, open and admitted for the same admission on another
    /// handle: this one holds it too, with that Login's reply. No Open,
    /// op63 or Login was sent.
    Joined(LoginReply),
    /// No handle holds it: the name is this handle's to open, log in to,
    /// register and then [`Session::admit_file`].
    Open,
}

/// A hosted file open on the connection and the handles holding it: one
/// Open, op63 and Login for all of them, and its op62 when the last lets
/// go (#1011). FileMaker Pro has each file open once per connection
/// (network-protocol.md 5.5); a host keeps one entry per file id on a
/// connection, and a second Open of it replaces the first.
#[derive(Debug)]
struct HeldFile {
    /// Its hosted name as FileMaker compares names ([`held_name`]).
    name: String,
    /// The Open reply, once the host gave one.
    opened: Option<OpenedFile>,
    /// The client file id its op63 registered.
    client_file: Option<i32>,
    /// The reply of the Login that admitted it.
    login: Option<LoginReply>,
    /// Whom that Login admitted ([`Session::admit_file`]): until then no
    /// other handle holds it.
    admission: Option<Admission>,
    /// The handles holding it, the one that opened it first.
    holders: Vec<u64>,
    /// Its last holder let go and is closing it (op62): no handle holds
    /// it again until that is done ([`Session::close_file`]).
    closing: bool,
}

/// A hosted file's name as FileMaker compares names: without case, with
/// its `.fmp12`.
fn held_name(filename: &str) -> String {
    let name = filename.trim().to_lowercase();
    if name.ends_with(".fmp12") {
        name
    } else {
        format!("{name}.fmp12")
    }
}

impl Inbox {
    /// Keep the notification records the server sent: the administrator's
    /// messages apart from change notifications (one that does not read as
    /// a message is dropped).
    fn receive(&mut self, records: Vec<write::Notification>) {
        for record in records {
            if !notify::is_admin_code(record.code) {
                // No handle would take it: no file is open, or it names a
                // file that was closed.
                let named = notified_file(&record.message);
                if !self.open.is_empty() && !named.is_some_and(|file| self.retired.contains(&file))
                {
                    let takers = self.takers(named);
                    self.notifications.push((named, record, takers));
                }
            } else if let Some(message) = AdminMessage::parse(&record) {
                if self.admin_messages.len() == MAX_ADMIN_MESSAGES {
                    self.admin_messages.pop_front();
                }
                self.admin_messages.push_back(message);
            }
        }
    }

    /// The handle that takes what names no handle: the one whose file was
    /// opened first of those still open.
    fn first_handle(&self) -> Option<u64> {
        self.open.first().map(|(handle, _)| *handle)
    }

    /// The handles that take a notification naming client file id
    /// `named`: those holding that file, else those holding the file
    /// opened first.
    fn takers(&self, named: Option<i32>) -> Vec<u64> {
        let holding = |file: i32| -> Vec<u64> {
            self.open
                .iter()
                .filter(|(_, registered)| *registered == Some(file))
                .map(|(handle, _)| *handle)
                .collect()
        };
        if let Some(named) = named {
            let takers = holding(named);
            if !takers.is_empty() {
                return takers;
            }
        }
        match self.open.first() {
            Some((_, Some(file))) => holding(*file),
            Some((handle, None)) => vec![*handle],
            None => Vec::new(),
        }
    }

    /// The administrator's messages routed to handle `key`: its copy of
    /// each notice naming a file it holds, and those for the whole
    /// connection that no handle took yet ([`Wire::route_admin`]).
    fn take_admin(&mut self, key: u64) -> Vec<AdminMessage> {
        let mut mine = Vec::new();
        self.admin_routed
            .retain_mut(|(message, takers)| match takers {
                None => {
                    mine.push(message.clone());
                    false
                }
                Some(takers) => {
                    if let Some(at) = takers.iter().position(|taker| *taker == key) {
                        takers.remove(at);
                        mine.push(message.clone());
                    }
                    !takers.is_empty()
                }
            });
        mine
    }

    /// The handle that takes the result for `callback`.
    fn result_handle(&self, callback: u32) -> Option<u64> {
        self.callbacks
            .get(&callback)
            .map(|(handle, _)| *handle)
            .or_else(|| self.first_handle())
    }

    /// A Perform Script on Server result the host sent: kept unless the
    /// file that started it was closed.
    fn receive_result(&mut self, result: PsosResult) {
        if !self.retired_callbacks.remove(&result.callback) {
            self.psos_results.push_back(result);
        }
    }

    /// A connection-wide id for `handle`'s callback `own`.
    fn register_callback(&mut self, handle: u64, own: u32) -> u32 {
        // Not one in use, nor one whose result may still come for a
        // closed file.
        let mut id = self.next_callback.max(1);
        while self.callbacks.contains_key(&id) || self.retired_callbacks.contains(&id) {
            id = id.wrapping_add(1).max(1);
        }
        self.next_callback = id.wrapping_add(1).max(1);
        self.callbacks.insert(id, (handle, own));
        id
    }

    /// How many Perform Script on Server results wait for `handle`.
    fn results_for(&self, handle: u64) -> usize {
        self.psos_results
            .iter()
            .filter(|r| self.result_handle(r.callback) == Some(handle))
            .count()
    }

    /// `handle`'s file is closed: what waits for it is dropped, and what
    /// names its client file id or its calls' callback ids from now on.
    fn close(&mut self, handle: u64) {
        if !self.open.iter().any(|(h, _)| *h == handle) {
            return;
        }
        // What waits for it is not taken by it.
        for (_, _, takers) in &mut self.notifications {
            takers.retain(|taker| *taker != handle);
        }
        self.notifications
            .retain(|(_, _, takers)| !takers.is_empty());
        let results = std::mem::take(&mut self.psos_results);
        let (dropped, kept): (VecDeque<_>, VecDeque<_>) = results
            .into_iter()
            .partition(|r| self.result_handle(r.callback) == Some(handle));
        self.psos_results = kept;
        let at = self
            .open
            .iter()
            .position(|(h, _)| *h == handle)
            .expect("checked above");
        // A file another handle still holds is not retired.
        if let (_, Some(file)) = self.open.remove(at) {
            if !self
                .open
                .iter()
                .any(|(_, registered)| *registered == Some(file))
            {
                self.retired.insert(file);
            }
        }
        let ended: Vec<u32> = self
            .callbacks
            .iter()
            .filter(|(_, (sender, _))| *sender == handle)
            .map(|(id, _)| *id)
            .collect();
        for id in ended {
            self.callbacks.remove(&id);
            // A call whose result already came has nothing more to come.
            if !dropped.iter().any(|r| r.callback == id) {
                self.retired_callbacks.insert(id);
            }
        }
    }
}

/// The client file id a change notification's message names: its first
/// item, a 4-byte id after the message's 16-byte header (write-path.md
/// 6.3, `om_protocol::write::record_message`). `None` for a message that
/// does not have that form.
fn notified_file(message: &[u8]) -> Option<i32> {
    let items = u16::from_be_bytes(message.get(12..14)?.try_into().ok()?);
    let length = u16::from_be_bytes(message.get(20..22)?.try_into().ok()?);
    if items == 0 || length != 4 {
        return None;
    }
    Some(i32::from_be_bytes(message.get(16..20)?.try_into().ok()?))
}

/// FileMaker's error 3, "command is unavailable": the answer to a call this
/// client does not handle.
const UNAVAILABLE: u16 = 3;

/// How long a Tickle waits for its reply before the host is taken to be
/// gone: FileMaker Pro's 30-second call timeout around `Tickle`
/// (network-protocol.md 6.1).
pub const TICKLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How many of the host's messages one idle poll reads at most
/// ([`Session::try_take_admin_messages`]): a host that keeps sending does
/// not hold the caller.
const IDLE_MESSAGES: usize = 32;

/// How long an idle poll's answers to the host's calls may wait, together,
/// for the host to take them: the answers are small, so a write waits only
/// once the host has stopped reading.
const IDLE_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long an idle poll reads one message, from the start of its read: a
/// message already there is read at once; one arriving slowly is left for
/// the next read.
const IDLE_READ: Duration = Duration::from_millis(5);

/// The most slot bytes [`Session::get_file_list`] reads across a list's
/// pages: as much as one transport message may carry, so that a host
/// cannot make the client hold more by paging (OpenMaker Server lists at
/// most 10,000 names of at most 1,024 bytes).
const MAX_FILE_LIST: usize = crate::tls::MAX_MESSAGE_LEN;

/// How long an exchange waits for its reply.
#[derive(Clone, Copy, Debug)]
enum Wait {
    /// Until a read finds nothing for the transport's read timeout.
    Read,
    /// However long the reply takes (a script the request waits for).
    Patient,
    /// Until this moment, however much else the server sends meanwhile.
    Until(Instant),
}

/// A hosted file opened in this session.
#[derive(Clone, Debug)]
pub struct OpenedFile {
    /// The host name used to open the file.
    pub host: String,
    /// The hosted file name, e.g. `test project.fmp12`.
    pub filename: String,
    /// The Open (op78) reply: per-file nonce, server file id, and the rest.
    pub reply: OpenReply,
}

/// Errors during a session.
#[derive(Debug)]
pub enum SessionError {
    Transport(TransportError),
    Protocol(String),
    /// The connection to the host is gone: the transport failed, the host
    /// closed it or stopped answering. The session cannot be used again;
    /// the file has to be opened on a new one.
    ConnectionLost(String),
    /// The file is open on this connection for another account and
    /// password, or another handle is opening it: a second Open there
    /// would replace it (#1011). It opens on a connection of its own.
    FileHeld,
}

impl SessionError {
    /// Whether the session's connection is gone.
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, SessionError::ConnectionLost(_))
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Transport(e) => write!(f, "{e}"),
            SessionError::Protocol(e) => write!(f, "{e}"),
            SessionError::ConnectionLost(reason) => {
                write!(f, "the connection to the host was lost ({reason})")
            }
            SessionError::FileHeld => {
                write!(f, "the file is open on this connection for another account")
            }
        }
    }
}
impl std::error::Error for SessionError {}
impl From<TransportError> for SessionError {
    fn from(e: TransportError) -> Self {
        SessionError::Transport(e)
    }
}

/// The op14 payload [`Session::upload_records`] sends for `records`, or why
/// it cannot go in one request. A save of several files checks every file's
/// before any of them sends record data.
pub fn upload_payload(records: Vec<OwnedNode>) -> Result<Vec<u8>, SessionError> {
    let payload = write::upload_payload(records)
        .ok_or_else(|| SessionError::Protocol("record data too large for a slot".into()))?;
    if payload.len() > PAGE_LIMIT {
        return Err(SessionError::Protocol(
            "record upload larger than one request; multi-part uploads are not supported".into(),
        ));
    }
    Ok(payload)
}

/// The op70 commit list [`Session::commit_tables`] sends for `tables`, or
/// why it cannot go in one request ([`upload_payload`]).
pub fn commit_list(tables: Vec<(Vec<u8>, Vec<OwnedNode>)>) -> Result<Vec<u8>, SessionError> {
    let list = write::commit_list_tables(tables)
        .ok_or_else(|| SessionError::Protocol("commit list too large for a slot".into()))?;
    if list.len() > PAGE_LIMIT {
        return Err(SessionError::Protocol(
            "commit list larger than one request; multi-part uploads are not supported".into(),
        ));
    }
    Ok(list)
}

impl Session {
    /// Open the transport, run `_is_a` + `Connect`, and return a live session.
    ///
    /// `info` carries the machine info; leave `install_code` empty to connect
    /// without a license key.
    pub fn connect(
        host: &str,
        port: u16,
        client_cert_pem: &[u8],
        client_key_pem: &[u8],
        callback_host: &str,
        info: &MachineInfo,
    ) -> Result<Self, SessionError> {
        let transport = Transport::connect(host, port, client_cert_pem, client_key_pem)?;
        Self::handshake(transport, callback_host, info)
    }

    /// Connect with certificate-chain and hostname validation for normal
    /// application use. Explicit private CAs may be supplied in PEM form.
    #[allow(clippy::too_many_arguments)]
    pub fn connect_verified(
        host: &str,
        port: u16,
        client_cert_pem: &[u8],
        client_key_pem: &[u8],
        callback_host: &str,
        info: &MachineInfo,
        additional_ca_pem: &[u8],
    ) -> Result<Self, SessionError> {
        let transport = Transport::connect_verified(
            host,
            port,
            client_cert_pem,
            client_key_pem,
            additional_ca_pem,
        )?;
        Self::handshake(transport, callback_host, info)
    }

    fn handshake(
        transport: Transport,
        callback_host: &str,
        info: &MachineInfo,
    ) -> Result<Self, SessionError> {
        let now = Instant::now();
        let mut wire = Wire {
            transport: Some(transport),
            connection_id: 0,
            next_request_id: 2,
            inbox: Inbox::default(),
            last_request: now,
            last_used: now,
            keepalive: Some(KEEPALIVE_INTERVAL),
            tickle_timeout: TICKLE_TIMEOUT,
            next_file_transaction: 1,
            next_transaction: 1,
            next_handle: 1,
            handles: 1,
            files: Vec::new(),
            revoked: BTreeSet::new(),
            lost: Arc::default(),
            closing: false,
        };
        // Advertise a BiDirIIOP listen point so the server runs its back-channel
        // over this connection; without it, Open fails (the server cannot set up
        // the session's back-channel).
        let contexts = [bidir_service_context(&[(callback_host, 5003)])];
        let is_a = wire.exchange(
            |id| build_is_a(id, &contexts),
            Wait::Read,
            Carries::NoRecords,
        )?;
        let is_a = Reply::parse(&is_a).map_err(|e| SessionError::Protocol(format!("{e}")))?;
        if is_a.body.first().copied().unwrap_or(0) == 0 {
            return Err(SessionError::Protocol("server is not RCFMP12".into()));
        }

        let connect_msg = wire.exchange(
            |id| build_connect(id, callback_host, 0x138b, info),
            Wait::Read,
            Carries::NoRecords,
        )?;
        let identity = parse_connect_reply(&connect_msg)
            .map_err(|e| SessionError::Protocol(format!("connect reply: {e}")))?;
        wire.connection_id = identity.connection_id;

        Ok(Self {
            link: Arc::new(Link {
                lost: Arc::clone(&wire.lost),
                wire: Mutex::new(wire),
                wake: Condvar::new(),
                keepalive: Mutex::new(None),
                successor: Mutex::new(None),
            }),
            connection_id: identity.connection_id,
            identity,
            key: 0,
            opened: None,
            client_file: None,
            refused_open: None,
            protocol_version: None,
        })
    }

    /// Another handle on this connection, with no file open: open another
    /// file on it, as FileMaker Pro opens a file's related files in the
    /// session it already has (network-protocol.md 5.5). The handle opens,
    /// logs in to and closes its own file; the connection, its keepalive
    /// and its counters are shared, and it ends with its last handle.
    pub fn share(&self) -> Session {
        Self::handle_on(
            Arc::clone(&self.link),
            self.connection_id,
            self.identity.clone(),
        )
    }

    /// A new handle, with no file open, on connection `link`.
    fn handle_on(link: Arc<Link>, connection_id: u32, identity: ConnectReply) -> Session {
        let key = {
            let mut wire = link.lock();
            wire.handles += 1;
            let key = wire.next_handle;
            wire.next_handle += 1;
            key
        };
        Session {
            link,
            connection_id,
            identity,
            key,
            opened: None,
            client_file: None,
            refused_open: None,
            protocol_version: None,
        }
    }

    /// This lost connection's file reconnected on `to`, to `host` at
    /// `port`, for `admission`: the other files that were on this
    /// connection may reopen there ([`Self::successor`]). Nothing when this
    /// connection is not lost.
    pub fn hand_over(&self, to: &Session, host: &str, port: u16, admission: Admission) {
        if self.connection_lost().is_none() {
            return;
        }
        *self
            .link
            .successor
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Successor {
            link: Arc::downgrade(&to.link),
            connection_id: to.connection_id,
            identity: to.identity.clone(),
            host: host.to_owned(),
            port,
            admission,
        });
    }

    /// A handle, with no file open, on the connection another file of this
    /// lost connection reconnected on ([`Self::hand_over`]), for a file on
    /// `host` at `port` (the host's text in any case) admitted for
    /// `admission`: `None` when this connection is not lost, no file of it
    /// reconnected, that was another host, port or admission, or the new
    /// connection is gone or lost too.
    pub fn successor(&self, host: &str, port: u16, admission: &Admission) -> Option<Session> {
        self.connection_lost()?;
        let successor = self
            .link
            .successor
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let successor = successor.as_ref().filter(|next| {
            next.port == port
                && next.host.eq_ignore_ascii_case(host)
                && next.admission == *admission
        })?;
        let link = successor.link.upgrade()?;
        if link.lost.get().is_some() {
            return None;
        }
        Some(Self::handle_on(
            link,
            successor.connection_id,
            successor.identity.clone(),
        ))
    }

    /// The connection this handle is on.
    pub fn connection(&self) -> ConnectionRef {
        ConnectionRef(Arc::downgrade(&self.link))
    }

    /// The client file id this handle's op63 registered its file under.
    pub fn client_file_id(&self) -> Option<i32> {
        self.client_file
    }

    /// How many files are open on the connection, by all its handles.
    pub fn files_open(&self) -> usize {
        self.link.lock().inbox.open.len()
    }

    /// The next write transaction id (locks, uploads, commits;
    /// write-path.md 3): one sequence for the connection and all its
    /// handles, since a host keys a lock's holder by connection and
    /// transaction. Never 0.
    pub fn take_transaction(&mut self) -> u32 {
        let mut wire = self.link.lock();
        let id = wire.next_transaction;
        wire.next_transaction = wire.next_transaction.wrapping_add(1).max(1);
        id
    }

    /// The server identity from the Connect reply.
    pub fn identity(&self) -> &ConnectReply {
        &self.identity
    }

    /// The 16-byte connection key from the Connect reply (the login obscuring salt).
    pub fn connection_key(&self) -> &[u8; 16] {
        &self.identity.connection_key
    }

    /// Whether the host's object is also of type `repo_id`: CORBA's
    /// `_is_a`, how a client asks a host for an interface before using it.
    /// A host that answers with an exception does not claim the interface.
    pub fn is_a(&mut self, repo_id: &[u8]) -> Result<bool, SessionError> {
        let msg = self.call(
            |id| build_is_a_of(id, repo_id, &[]),
            false,
            Carries::NoRecords,
        )?;
        let reply = Reply::parse(&msg).map_err(|e| SessionError::Protocol(format!("{e}")))?;
        if reply.status != ReplyStatus::NoException {
            return Ok(false);
        }
        match reply.body.first() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(SessionError::Protocol(
                "_is_a reply is not a boolean".into(),
            )),
        }
    }

    /// The names of the files the host publishes in OpenMaker's file list
    /// (`om_protocol::hosted_files`), in its order. Only for a host that
    /// claimed [`hosted_files::REPO_ID`] ([`Session::is_a`]). A reply that
    /// is not a complete, well-formed list is an error, never a shorter
    /// list.
    pub fn list_hosted_files(&mut self) -> Result<Vec<String>, SessionError> {
        let connection_id = self.connection_id;
        let msg = self.call(
            |id| hosted_files::build_list_files(id, connection_id),
            false,
            Carries::NoRecords,
        )?;
        hosted_files::parse_list_files_reply(&msg)
            .map_err(|e| SessionError::Protocol(format!("file list: {e}")))
    }

    /// List the host's files with GetFileList (op77), anonymously, as
    /// FileMaker Pro's Hosts window does (network-protocol.md 5.3): no file
    /// is opened and no Login sent. A list on several pages is read whole:
    /// its tree is the pages' slots in order, its count, host name and
    /// error the first page's. A reply with an error is returned as it
    /// came; one that cannot be read, or a list of more than 64 MiB, is an
    /// error.
    pub fn get_file_list(&mut self) -> Result<om_protocol::FileListReply, SessionError> {
        self.get_file_list_with_max(om_protocol::DEFAULT_MAX_RETURN)
    }

    /// [`Session::get_file_list`] with an explicit `maxreturn`; a smaller
    /// value makes the server page sooner.
    pub fn get_file_list_with_max(
        &mut self,
        max_return: u32,
    ) -> Result<om_protocol::FileListReply, SessionError> {
        let mut request = om_protocol::FileListRequest::anonymous();
        let mut list: Option<om_protocol::FileListReply> = None;
        loop {
            let transaction = om_protocol::build_file_list(1, &request);
            let result = self.perform_with_max(&transaction, max_return)?;
            let page = om_protocol::parse_file_list_reply(&result)
                .map_err(|e| SessionError::Protocol(format!("file list reply: {e}")))?;
            if page.error != 0 {
                return Ok(page);
            }
            check_progress(&page.resume, page.slots.len())?;
            let resume = page.resume.clone();
            let whole = match list.take() {
                None => page,
                Some(mut whole) => {
                    if whole.slots.len() + page.slots.len() > MAX_FILE_LIST {
                        return Err(SessionError::Protocol(format!(
                            "file list longer than {MAX_FILE_LIST} bytes"
                        )));
                    }
                    whole.slots.extend_from_slice(&page.slots);
                    om_protocol::FileListReply {
                        resume: page.resume,
                        ..whole
                    }
                }
            };
            if !resume.has_more() {
                return Ok(whole);
            }
            list = Some(whole);
            request.resume = resume;
        }
    }

    /// The connection's local address: the client's address as the server
    /// sees it.
    pub fn local_addr(&self) -> Result<SocketAddr, SessionError> {
        Ok(self.link.lock().transport()?.local_addr()?)
    }

    /// Why the connection to the host was lost, once it is: by a request
    /// of this session, or by the keepalive while the session was idle.
    /// It does not wait for an exchange in progress.
    pub fn connection_lost(&self) -> Option<String> {
        self.link.lost.get().cloned()
    }

    /// Whether the host disconnected this handle's file (`0x0870`) and the
    /// connection let go of it ([`HOST_DISCONNECTED_FILE`]): what is asked
    /// of it fails, while the connection's other files go on. Not waiting
    /// behind another exchange (a Tickle), as idle callers ask: the next
    /// ask sees it.
    pub fn file_disconnected(&self) -> bool {
        let Some(file) = self.opened.as_ref().map(|opened| opened.reply.file_id) else {
            return false;
        };
        self.link
            .try_lock()
            .is_some_and(|wire| wire.revoked.contains(&file))
    }

    /// How long the connection may stay silent while a file is open before
    /// the keepalive sends a Tickle ([`KEEPALIVE_INTERVAL`], as FileMaker Pro;
    /// tests use shorter ones). `None` stops the Tickles.
    pub fn set_keepalive(&mut self, interval: Option<Duration>) {
        self.link.lock().keepalive = interval;
        self.link.wake.notify_all();
    }

    /// How long a Tickle waits for its reply before the connection counts
    /// as lost ([`TICKLE_TIMEOUT`]; tests use shorter ones). The wait ends
    /// then even if the host keeps sending calls of its own meanwhile.
    pub fn set_tickle_timeout(&mut self, timeout: Duration) {
        self.link.lock().tickle_timeout = timeout;
    }

    /// Send one Tickle now, reporting the open file's idle time (the
    /// keepalive does this by itself).
    pub fn tickle(&mut self) -> Result<(), SessionError> {
        self.link.lock().tickle()
    }

    /// One exchange of this session's own: the request `build` makes with
    /// the next request id, and its reply. `patient` waits past the read
    /// timeout.
    fn call(
        &mut self,
        build: impl FnOnce(u32) -> Vec<u8>,
        patient: bool,
        carries: Carries,
    ) -> Result<Vec<u8>, SessionError> {
        let wait = if patient { Wait::Patient } else { Wait::Read };
        let mut wire = self.link.lock();
        // A file the host disconnected is let go of before anything else
        // goes out.
        wire.route_admin(true);
        let reply = wire.call(build, wait, carries);
        wire.last_used = wire.last_request;
        reply
    }

    /// Start the connection's keepalive, once, when its first file opens.
    fn keep_alive(&mut self) {
        let mut thread = self
            .link
            .keepalive
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if thread.is_none() {
            let link = Arc::clone(&self.link);
            *thread = std::thread::Builder::new()
                .name("om-session keepalive".into())
                .spawn(move || keepalive(&link))
                .ok();
        }
    }

    /// End the keepalive and take the transport: nothing goes out on it
    /// after this but what the caller sends.
    fn stop(&mut self) -> Option<Transport> {
        let transport = {
            let mut wire = self.link.lock();
            wire.closing = true;
            wire.transport.take()
        };
        self.link.wake.notify_all();
        let thread = self
            .link
            .keepalive
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
        transport
    }

    /// The file opened by [`Session::open_file`], if any.
    pub fn opened_file(&self) -> Option<&OpenedFile> {
        self.opened.as_ref()
    }

    /// The status the host answered the last [`Session::open_file`] with,
    /// when it refused the Open (spec 5.5: 4 the file is missing, 0x4fb3
    /// it cannot be opened, 0x507c an error with a message): `None` after
    /// an Open it accepted or one that failed before the host answered.
    pub fn refused_open(&self) -> Option<u16> {
        self.refused_open
    }

    /// Open a hosted file (op78). Read-only; returns the per-file nonce + status
    /// and remembers the file for [`Session::login`]. Once the host opened
    /// it, the connection's keepalive runs until its last handle
    /// disconnects.
    pub fn open_file(&mut self, host: &str, filename: &str) -> Result<OpenReply, SessionError> {
        // One Open of a file per connection (#1011): not again on this
        // handle, which would replace it on the host under any other handle
        // holding it, and not while another handle holds or opens it,
        // which [`Session::claim_file`] is for.
        {
            let name = held_name(filename);
            let key = self.key;
            let mut wire = self.link.lock();
            if wire
                .held(key, &name)
                .is_some_and(|file| file.opened.is_some())
            {
                return Err(SessionError::Protocol(
                    "this handle has the file open already".into(),
                ));
            }
            if wire
                .files
                .iter()
                .any(|file| file.name == name && !file.holders.contains(&key))
            {
                return Err(SessionError::FileHeld);
            }
            if wire.held(key, &name).is_none() {
                wire.files.push(HeldFile {
                    name,
                    opened: None,
                    client_file: None,
                    login: None,
                    admission: None,
                    holders: vec![key],
                    closing: false,
                });
            }
        }
        self.refused_open = None;
        let connection_id = self.connection_id;
        let msg = self.call(
            |id| build_open(id, connection_id, host, filename),
            false,
            Carries::NoRecords,
        )?;
        let perform = om_protocol::parse_perform_reply(&msg)
            .map_err(|e| SessionError::Protocol(format!("open perform reply: {e}")))?;
        let reply = parse_open_reply(&perform.result).map_err(|e| {
            if let om_protocol::PerformError::Server(status)
            | om_protocol::PerformError::OpenError(status, _) = e
            {
                self.refused_open = Some(status);
            }
            SessionError::Protocol(format!("open reply: {e}"))
        })?;
        self.opened = Some(OpenedFile {
            host: host.to_string(),
            filename: filename.to_string(),
            reply: reply.clone(),
        });
        if reply.status == 0 {
            {
                let mut wire = self.link.lock();
                let key = self.key;
                if !wire.inbox.open.iter().any(|(handle, _)| *handle == key) {
                    wire.inbox.open.push((key, self.client_file));
                }
                if let Some(file) = wire.held(key, &held_name(filename)) {
                    file.opened = self.opened.clone();
                }
            }
            self.keep_alive();
        }
        Ok(reply)
    }

    /// Hold `filename` on this handle (#1011): when another handle on the
    /// connection holds it, open and admitted for `admission`, this one
    /// holds it too ([`Claim::Joined`]), with no Open, op63 or Login, and
    /// its requests and notifications are that file's; when none does, it
    /// is this handle's to open ([`Claim::Open`]), and no other handle
    /// opens it meanwhile. [`SessionError::FileHeld`]: held for another
    /// admission, or still being opened; it opens on a connection of its
    /// own.
    pub fn claim_file(
        &mut self,
        filename: &str,
        admission: &Admission,
    ) -> Result<Claim, SessionError> {
        if let Some(reason) = self.connection_lost() {
            return Err(SessionError::ConnectionLost(reason));
        }
        if self.opened.is_some() {
            return Err(SessionError::Protocol(
                "this handle has a file open already".into(),
            ));
        }
        let name = held_name(filename);
        let key = self.key;
        let mut wire = self.link.lock();
        let Some(file) = wire.files.iter_mut().find(|file| file.name == name) else {
            wire.files.push(HeldFile {
                name,
                opened: None,
                client_file: None,
                login: None,
                admission: None,
                holders: vec![key],
                closing: false,
            });
            return Ok(Claim::Open);
        };
        if file.closing {
            return Err(SessionError::FileHeld);
        }
        if file.holders.contains(&key) {
            return Ok(Claim::Open);
        }
        let (Some(opened), Some(login), Some(admitted)) =
            (&file.opened, &file.login, &file.admission)
        else {
            return Err(SessionError::FileHeld);
        };
        if admitted != admission {
            return Err(SessionError::FileHeld);
        }
        let (opened, login, client_file) = (opened.clone(), login.clone(), file.client_file);
        file.holders.push(key);
        if !wire.inbox.open.iter().any(|(handle, _)| *handle == key) {
            wire.inbox.open.push((key, client_file));
        }
        drop(wire);
        self.opened = Some(opened);
        self.client_file = client_file;
        self.protocol_version = login.version;
        self.keep_alive();
        Ok(Claim::Joined(login))
    }

    /// The file this handle opened, logged in to and registered is
    /// `admission`'s: another handle claiming it for the same admission
    /// holds it too ([`Session::claim_file`]).
    pub fn admit_file(&mut self, admission: Admission) -> Result<(), SessionError> {
        let key = self.key;
        let name = self.held_file_name();
        let mut wire = self.link.lock();
        let file = name
            .and_then(|name| wire.held(key, &name))
            .filter(|file| file.opened.is_some() && file.login.is_some())
            .ok_or_else(|| SessionError::Protocol("admit requires a logged-in file".into()))?;
        file.admission = Some(admission);
        Ok(())
    }

    /// Log in to the opened file with a local FileMaker account (op80).
    ///
    /// `internal_creation` is the file's `mInternalCreation` (read from the
    /// file's HBAM, spec 4.3); with the Open nonce and the connection key it
    /// derives the per-login key that obscures the credentials. Sends exactly
    /// one Login; a failed attempt counts toward the server's account lockout,
    /// so callers must not retry blindly. Returns the server's reply; a
    /// rejected login is a reply with a non-zero `error_code`, not an `Err`.
    pub fn login(
        &mut self,
        account: &str,
        password: &str,
        internal_creation: u32,
        environment: &LoginEnvironment,
    ) -> Result<LoginReply, SessionError> {
        let opened = self
            .opened
            .as_ref()
            .ok_or_else(|| SessionError::Protocol("login requires an opened file".into()))?;
        let request = LoginRequest {
            transaction_id: self.link.lock().take_file_transaction(),
            file_id: opened.reply.file_id,
            open_nonce: opened.reply.file_nonce,
            internal_creation,
            connection_key: self.identity.connection_key,
            account,
            password,
            environment,
        };
        let transaction = build_login(&request);
        let result = self.perform(&transaction)?;
        let reply = parse_login_reply(&result)
            .map_err(|e| SessionError::Protocol(format!("login reply: {e}")))?;
        if reply.is_success() {
            self.protocol_version = reply.version;
            let key = self.key;
            if let Some(name) = self.held_file_name() {
                if let Some(file) = self.link.lock().held(key, &name) {
                    file.login = Some(reply.clone());
                }
            }
        }
        Ok(reply)
    }

    /// The protocol version the server named when the login succeeded (9
    /// from FileMaker Server 26), which fixes the layout of some requests.
    pub fn protocol_version(&self) -> Option<u16> {
        self.protocol_version
    }

    /// Read the opened file's `mInternalCreation` the way the official client
    /// does: download the `[2]` subtree (op90) and take the value at `[2, 254]`.
    /// This is the second half of the Login nonce (spec 4.3).
    pub fn fetch_internal_creation(&mut self) -> Result<u32, SessionError> {
        self.fetch_status().map(|(creation, _)| creation)
    }

    /// [`Session::fetch_internal_creation`], with the `[2]` subtree it read.
    /// FileMaker Pro 26 reads `[2]` once, before Login, and uses that for the
    /// session (network-protocol.md 5.6.9b): a caller may keep the reply for
    /// the file's locale and options rather than download `[2]` again.
    pub fn fetch_status(&mut self) -> Result<(u32, DownloadReply), SessionError> {
        let reply = self.download(&[&[2]])?;
        if reply.lock_error != 0 {
            return Err(SessionError::Protocol(format!(
                "download of [2] failed with lock error 0x{:04x}",
                reply.lock_error
            )));
        }
        let tree = om_hbam::status_tree_from_wire(&reply.slots)
            .map_err(|e| SessionError::Protocol(format!("decode [2] subtree: {e}")))?;
        let creation = tree
            .internal_creation()
            .map_err(|e| SessionError::Protocol(format!("read [2, 254]: {e}")))?
            .ok_or_else(|| SessionError::Protocol("file has no [2, 254] value".into()))?;
        Ok((creation, reply))
    }

    /// Download one key path of the opened file's HBAM tree (op90), following
    /// the server's pages until the subtree is complete (spec 5.6.6c). The
    /// reply's slots are the whole stream; a key that holds a value comes back
    /// in [`DownloadReply::value`] instead.
    pub fn download(&mut self, key_path: &[&[u8]]) -> Result<DownloadReply, SessionError> {
        self.download_with_max(key_path, DOWNLOAD_MAX_RETURN)
    }

    /// [`Session::download`] with an explicit `maxreturn`. The official client
    /// always uses [`DOWNLOAD_MAX_RETURN`]; a smaller value makes the server
    /// page sooner.
    pub fn download_with_max(
        &mut self,
        key_path: &[&[u8]],
        max_return: u32,
    ) -> Result<DownloadReply, SessionError> {
        let file_id = self.opened_file_id()?;
        let mut resume = Resume::start();
        let mut slots = Vec::new();
        loop {
            let transaction = build_download(&DownloadRequest {
                transaction_id: self.take_file_transaction(),
                file_id,
                key_path,
                resume,
            });
            let result = self.perform_with_max(&transaction, max_return)?;
            let reply = parse_download_reply(&result)
                .map_err(|e| SessionError::Protocol(format!("download reply: {e}")))?;
            check_progress(&reply.resume, reply.slots.len())?;
            slots.extend_from_slice(&reply.slots);
            if !reply.resume.has_more() {
                return Ok(DownloadReply { slots, ..reply });
            }
            resume = reply.resume;
        }
    }

    /// Download selected children of one key path of the opened file (op92),
    /// e.g. records by id under `[table, 5]`, following the server's pages. The
    /// reply holds the requested children that exist; decode its slots with
    /// `om_hbam` relative to `key_path`. `flag` is the request's last byte (0
    /// for record lists).
    pub fn download_list(
        &mut self,
        key_path: &[&[u8]],
        keys: &[&[u8]],
        flag: u8,
    ) -> Result<DownloadListReply, SessionError> {
        let file_id = self.opened_file_id()?;
        let mut resume = Resume::start();
        let mut slots = Vec::new();
        loop {
            let transaction = build_download_list(&DownloadListRequest {
                transaction_id: self.take_file_transaction(),
                file_id,
                key_path,
                keys,
                flag,
                resume,
            });
            let result = self.perform_with_max(&transaction, DOWNLOAD_LIST_MAX_RETURN)?;
            let reply = parse_download_list_reply(&result)
                .map_err(|e| SessionError::Protocol(format!("download list reply: {e}")))?;
            check_progress(&reply.resume, reply.slots.len())?;
            slots.extend_from_slice(&reply.slots);
            if !reply.resume.has_more() {
                return Ok(DownloadListReply { slots, ..reply });
            }
            resume = reply.resume;
        }
    }

    /// Run a query program (op86) on the opened file and return its result
    /// spots as one slot stream, following the server's pages. The program's
    /// instructions must carry this session's access keys (see
    /// [`Session::connection_key`] and `om_crypto::query_access_key`).
    ///
    /// # Panics
    /// If the program is larger than one request (`PAGE_LIMIT` bytes); this
    /// client does not upload programs in parts.
    pub fn query(&mut self, program: &[u8]) -> Result<QueryReply, SessionError> {
        assert!(program.len() <= PAGE_LIMIT, "program fits in one request");
        let file_id = self.opened_file_id()?;
        let mut resume = Resume::start();
        let mut slots = Vec::new();
        loop {
            let payload = if resume.has_more() { &[][..] } else { program };
            let transaction = build_query(&QueryRequest {
                transaction_id: self.take_file_transaction(),
                file_id,
                program: payload,
                resume,
            });
            let result = self.perform_with_max(&transaction, QUERY_MAX_RETURN)?;
            let reply = parse_query_reply(&result)
                .map_err(|e| SessionError::Protocol(format!("query reply: {e}")))?;
            check_progress(&reply.resume, reply.slots.len())?;
            slots.extend_from_slice(&reply.slots);
            if !reply.resume.has_more() {
                return Ok(QueryReply { slots, ..reply });
            }
            resume = reply.resume;
        }
    }

    /// The opened file's server id.
    fn opened_file_id(&self) -> Result<u32, SessionError> {
        let file_id = self
            .opened
            .as_ref()
            .ok_or_else(|| SessionError::Protocol("this request requires an opened file".into()))?
            .reply
            .file_id;
        // The host disconnected the file: nothing more is asked of it.
        if self.link.lock().revoked.contains(&file_id) {
            return Err(SessionError::ConnectionLost(HOST_DISCONNECTED_FILE.into()));
        }
        Ok(file_id)
    }

    /// The next file-transaction id (header transid-low), from the
    /// connection's one counter.
    fn take_file_transaction(&mut self) -> u32 {
        self.link.lock().take_file_transaction()
    }

    /// Perform one transaction on the opened file, built from the next
    /// file-transaction id and the file's id.
    fn perform_on_file(
        &mut self,
        build: impl FnOnce(u32, u32) -> Vec<u8>,
    ) -> Result<Vec<u8>, SessionError> {
        let file_id = self.opened_file_id()?;
        let transaction = build(self.take_file_transaction(), file_id);
        self.perform(&transaction)
    }

    /// Register this opened file for change notifications (op63,
    /// write-path.md 6.1). Its client file id, which the host's
    /// notifications for it name, is `-(transidlo + 2)` of this op63, as
    /// in every op63 of FileMaker Pro's 19-file connection (s03, #740):
    /// so the files of one connection get different ids.
    pub fn request_notifications(&mut self) -> Result<(), SessionError> {
        let mut registered = None;
        let result = self.perform_on_file(|id, file_id| {
            let client_file_id = -(id as i32).saturating_add(2);
            registered = Some(client_file_id);
            om_protocol::build_request_notification(
                id,
                &om_protocol::RequestNotification {
                    attention_mask: u32::MAX,
                    message_address: FILE_SESSION_ID,
                    path: Vec::new(),
                    file_id,
                    client_file_id,
                },
            )
        })?;
        if result != [0, 0] {
            return Err(SessionError::Protocol(
                "the host refused change notifications".into(),
            ));
        }
        self.client_file = registered;
        let mut wire = self.link.lock();
        let key = self.key;
        if let Some(file) = self
            .opened
            .as_ref()
            .and_then(|opened| wire.held(key, &held_name(&opened.filename)))
        {
            file.client_file = registered;
        }
        if let Some(entry) = wire
            .inbox
            .open
            .iter_mut()
            .find(|(handle, _)| *handle == key)
        {
            entry.1 = registered;
        }
        Ok(())
    }

    /// Reserve one new record id in base table `table` (op71, write-path.md
    /// 3.1), locked for transaction `txn`. The record exists only once it is
    /// uploaded and committed under the same `txn`; a reply with a non-zero
    /// status is returned, not an `Err`.
    pub fn create_record(
        &mut self,
        table: &[u8],
        txn: u32,
    ) -> Result<write::CreateRecordReply, SessionError> {
        let request = write::CreateRecord {
            table: vec![table.to_vec()],
            count: 1,
            txn,
        };
        let result =
            self.perform_on_file(|id, file| write::build_create_record(id, file, &request))?;
        write::parse_create_record_reply(&result)
            .map_err(|e| SessionError::Protocol(format!("create record reply: {e}")))
    }

    /// Upload whole records of base table `table` into this connection's
    /// temporary area (op14, write-path.md 3.3), for a following
    /// [`Session::commit_records`]. Each node is one record keyed by its id,
    /// holding one leaf per stored field.
    pub fn upload_records(
        &mut self,
        table: &[u8],
        records: Vec<OwnedNode>,
    ) -> Result<(), SessionError> {
        let payload = upload_payload(records)?;
        let request = write::Upload {
            payload,
            resume: Resume::start(),
            terminal: Vec::new(),
            flags: write::UPLOAD_FLAGS,
            dest: vec![table.to_vec(), vec![RECORDS]],
        };
        let result = self.perform_on_file(|id, file| write::build_upload(id, file, &request))?;
        complete_upload(&result, "upload")
    }

    /// Commit records of base table `table` from the temporary area (op70,
    /// write-path.md 3.4) under transaction `txn`. Each entry is keyed by
    /// record id: a leaf [`write::COMMIT_NEW`] or [`write::COMMIT_DELETED`],
    /// or a node listing the ids of the changed fields. The reply carries no
    /// commit status: read the records back to know what the server applied.
    pub fn commit_records(
        &mut self,
        table: &[u8],
        entries: Vec<OwnedNode>,
        txn: u32,
    ) -> Result<(), SessionError> {
        self.commit_tables(vec![(table.to_vec(), entries)], txn)
    }

    /// Commit records of several base tables in one op70 (a script
    /// transaction's commit), each table's entries as for
    /// [`Session::commit_records`].
    pub fn commit_tables(
        &mut self,
        tables: Vec<(Vec<u8>, Vec<OwnedNode>)>,
        txn: u32,
    ) -> Result<(), SessionError> {
        let list = commit_list(tables)?;
        let request = write::Commit {
            list,
            resume: Resume::start(),
            flags: write::COMMIT_COMPLETE_BEFORE_RETURNING,
            txn,
        };
        let result = self.perform_on_file(|id, file| write::build_commit(id, file, &request))?;
        complete_upload(&result, "commit")
    }

    /// Read and change a field's next serial value (op72, write-path.md 3.2):
    /// `flags` [`write::SERIAL_REPLACE`] stores `value`,
    /// [`write::SERIAL_INCREMENT`] advances it by `increment`. The reply
    /// holds the value before the change and a status (0, 102 field
    /// missing, 303 schema locked).
    pub fn get_serial(
        &mut self,
        table: &[u8],
        field: &[u8],
        value: &str,
        increment: i32,
        flags: u32,
    ) -> Result<write::GetSerialReply, SessionError> {
        let request = write::GetSerial {
            table: vec![table.to_vec()],
            field: vec![field.to_vec()],
            value: value.to_owned(),
            increment,
            flags,
        };
        let result =
            self.perform_on_file(|id, file| write::build_get_serial(id, file, &request))?;
        write::parse_get_serial_reply(&result)
            .map_err(|e| SessionError::Protocol(format!("serial reply: {e}")))
    }

    /// Take a write lock (type 9) on `path` for transaction `txn` without
    /// waiting (op60, write-path.md 4.2): `[T, 05, R]` to edit or delete a
    /// record, `[T, 01, F]` to check a unique field. With `want_mod_count`
    /// a granted lock carries the record's modification count (5.4).
    pub fn lock(
        &mut self,
        path: &[&[u8]],
        txn: u32,
        want_mod_count: bool,
    ) -> Result<write::LockReply, SessionError> {
        let file_id = self.opened_file_id()?;
        let request = write::Lock {
            waiter: 0,
            timeout: 0,
            want_mod_count,
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            argument: if want_mod_count { file_id as i32 } else { 0 },
            host_file: file_id as i32,
            txn,
            lock_type: write::LOCK_WRITE,
        };
        let result = self.perform_on_file(|id, file| write::build_lock(id, file, &request))?;
        write::parse_lock_reply(&result, want_mod_count)
            .map_err(|e| SessionError::Protocol(format!("lock reply: {e}")))
    }

    /// Take a lock of type `lock_type` on `path` for transaction `txn`,
    /// waiting up to `timeout` (op60, schema-path.md 4.1): Manage Database's
    /// `[08]` (type 9, timeout `ffffffff`), a catalog (9, 0), a table's
    /// records `[T, 05]` for a save (9, 2) and a table being deleted
    /// `[T]` (11, `ffffffff`). The host may answer at once whatever the
    /// timeout; a refusal is returned, not an `Err`.
    pub fn lock_typed(
        &mut self,
        path: &[&[u8]],
        txn: u32,
        lock_type: u16,
        timeout: u32,
    ) -> Result<write::LockReply, SessionError> {
        let file_id = self.opened_file_id()?;
        let request = write::Lock {
            waiter: 0,
            timeout,
            want_mod_count: false,
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            argument: 0,
            host_file: file_id as i32,
            txn,
            lock_type,
        };
        let result = self.perform_on_file(|id, file| write::build_lock(id, file, &request))?;
        write::parse_lock_reply(&result, false)
            .map_err(|e| SessionError::Protocol(format!("lock reply: {e}")))
    }

    /// Release transaction `txn`'s lock of `lock_type` on `path` (op62,
    /// write-path.md 4.3), telling the other clients registered on the
    /// file with `attention` (2 for a catalog that changed, 0 for none;
    /// schema-path.md 9).
    pub fn unlock(
        &mut self,
        path: &[&[u8]],
        txn: u32,
        lock_type: u16,
        attention: u32,
    ) -> Result<(), SessionError> {
        let file_id = self.opened_file_id()?;
        let request = write::Unlock {
            message_address: 0,
            attention_mask: attention,
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            host_file: file_id as i32,
            txn,
            lock_type,
        };
        self.perform_on_file(|id, file| write::build_unlock(id, file, &request))
            .map(|_| ())
    }

    /// Change transaction `txn`'s lock on `path` from `current` to
    /// `new` (op61, schema-path.md 4.2): a catalog's write lock to
    /// exclusive before it is replaced. Answered as op60.
    pub fn upgrade_lock(
        &mut self,
        path: &[&[u8]],
        txn: u32,
        current: u16,
        new: u16,
    ) -> Result<write::LockReply, SessionError> {
        let file_id = self.opened_file_id()?;
        let request = om_protocol::schema::Upgrade {
            waiter: 0,
            timeout: u32::MAX,
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            host_file: file_id as i32,
            txn,
            new_type: new,
            current_type: current,
        };
        let result = self
            .perform_on_file(|id, file| om_protocol::schema::build_upgrade(id, file, &request))?;
        write::parse_lock_reply(&result, false)
            .map_err(|e| SessionError::Protocol(format!("upgrade reply: {e}")))
    }

    /// The host's clock (op82, schema-path.md 3.1): its local date and
    /// time as the 8-byte date-time key modification stamps hold.
    pub fn host_timestamp(&mut self) -> Result<Vec<u8>, SessionError> {
        let result = self.perform_on_file(om_protocol::schema::build_host_timestamp)?;
        om_protocol::schema::parse_host_timestamp_reply(&result)
            .map_err(|e| SessionError::Protocol(format!("host time stamp reply: {e}")))
    }

    /// Empty the subtree at `dest` in the opened file (op13 with node state
    /// `00` and no payload), as FileMaker Pro deletes a script
    /// (scripts.md, "Saving scripts to a host"). The reply does not say
    /// what was stored: read it back to know.
    pub fn empty_on_host(&mut self, dest: &[&[u8]]) -> Result<(), SessionError> {
        let request = write::Upload {
            payload: Vec::new(),
            resume: Resume::start(),
            terminal: Vec::new(),
            flags: write::UPLOAD_EMPTY,
            dest: dest.iter().map(|segment| segment.to_vec()).collect(),
        };
        let result =
            self.perform_on_file(|id, file| write::build_upload_to_host(id, file, &request))?;
        let reply = write::parse_upload_reply(&result)
            .map_err(|e| SessionError::Protocol(format!("upload to host reply: {e}")))?;
        if reply.has_more() {
            return Err(SessionError::Protocol(
                "the host asked for more of an upload that was complete".into(),
            ));
        }
        Ok(())
    }

    /// Replace the subtree at `dest` in the opened file with `node`'s
    /// children (op13, schema-path.md 3.3), as Manage Database saves a
    /// catalog, in parts of whole chunks when it is larger than one
    /// request (network-protocol.md 5.6.6c). The reply does not say what
    /// was stored: read it back to know.
    pub fn upload_to_host(&mut self, dest: &[&[u8]], node: &OwnedNode) -> Result<(), SessionError> {
        let payload = node
            .to_wire()
            .ok_or_else(|| SessionError::Protocol("catalog too large for a slot".into()))?;
        let dest: Vec<Vec<u8>> = dest.iter().map(|segment| segment.to_vec()).collect();
        let mut resume = Resume::start();
        let (mut start, mut depth) = (0, 0);
        loop {
            let (end, after) = om_protocol::page_cut(&payload, start, PAGE_LIMIT, depth);
            let request = write::Upload {
                payload: payload[start..end].to_vec(),
                resume,
                terminal: Vec::new(),
                flags: write::UPLOAD_NODE,
                dest: dest.clone(),
            };
            let result =
                self.perform_on_file(|id, file| write::build_upload_to_host(id, file, &request))?;
            let reply = write::parse_upload_reply(&result)
                .map_err(|e| SessionError::Protocol(format!("upload to host reply: {e}")))?;
            (start, depth) = (end, after);
            match (start < payload.len(), reply.has_more()) {
                (false, false) => return Ok(()),
                (false, true) => {
                    return Err(SessionError::Protocol(
                        "the host asked for more of an upload that was complete".into(),
                    ))
                }
                (true, false) => {
                    return Err(SessionError::Protocol(format!(
                        "the host ended an upload after {start} of {} bytes",
                        payload.len()
                    )))
                }
                (true, true) => {}
            }
            resume = Resume { depth: -1, ..reply };
        }
    }

    /// Ask the host to do the field work listed for base table `table`
    /// (op73, schema-path.md 3.6, 7.2) after its field catalog was saved.
    pub fn update_table(&mut self, table: &[u8]) -> Result<(), SessionError> {
        let key = vec![table.to_vec()];
        self.perform_on_file(|id, file| om_protocol::schema::build_update_table(id, file, &key))
            .map(|_| ())
    }

    /// Close the opened file as FileMaker Pro does, when its user closes it
    /// or the host asks it to (network-protocol.md 6.5): one op62 Unlock of
    /// the file's root ([`write::Unlock::close_file`]), without a Logout.
    /// The handle stays connected with no file open; once no file is open
    /// on the connection, the keepalive's Tickles report no file session.
    pub fn close_file(&mut self) -> Result<(), SessionError> {
        // A file the host disconnected is let go of already
        // ([`Wire::route_admin`]): nothing is sent for it.
        let revoked = self
            .opened
            .as_ref()
            .is_some_and(|opened| self.link.lock().revoked.contains(&opened.reply.file_id));
        if revoked {
            self.forget_file();
            return Ok(());
        }
        let file_id = self.opened_file_id()?;
        // This handle lets go of the file, and only its last holder closes
        // it on the host, decided under the wire lock: while it does, no
        // handle holds the file again (#1011).
        let Some(name) = self.release_file() else {
            self.opened = None;
            self.client_file = None;
            return Ok(());
        };
        let request = write::Unlock::close_file(file_id as i32);
        let closed = self
            .perform_on_file(|id, file| write::build_unlock(id, file, &request))
            .map(|_| ());
        self.link
            .lock()
            .files
            .retain(|file| !(file.closing && file.name == name));
        self.opened = None;
        self.client_file = None;
        closed
    }

    /// The name this handle's open file is held by ([`held_name`]).
    fn held_file_name(&self) -> Option<String> {
        self.opened
            .as_ref()
            .map(|opened| held_name(&opened.filename))
    }

    /// Let go of this handle's file, under the wire lock: `Some` (its
    /// name, now closing) when this was its last holder, which closes it
    /// on the host; `None` when other handles still hold it, or it was
    /// never opened.
    fn release_file(&mut self) -> Option<String> {
        let key = self.key;
        let name = self.held_file_name();
        let mut wire = self.link.lock();
        wire.inbox.close(key);
        let at = wire
            .files
            .iter()
            .position(|file| Some(&file.name) == name.as_ref() && file.holders.contains(&key))?;
        let file = &mut wire.files[at];
        file.holders.retain(|holder| *holder != key);
        if !file.holders.is_empty() {
            return None;
        }
        if file.opened.is_none() {
            wire.files.remove(at);
            return None;
        }
        file.closing = true;
        Some(file.name.clone())
    }

    /// The handle's file is no longer open: what waits for it is dropped.
    fn forget_file(&mut self) {
        self.opened = None;
        self.client_file = None;
        let key = self.key;
        let mut wire = self.link.lock();
        wire.inbox.close(key);
        for file in &mut wire.files {
            file.holders.retain(|holder| *holder != key);
        }
        // A file another handle is closing stays until it has closed it.
        wire.files
            .retain(|file| !file.holders.is_empty() || file.closing);
    }

    /// Release every lock transaction `txn` holds on the opened file (op65,
    /// write-path.md 4.4), as FileMaker Pro does when the user leaves a record.
    pub fn kill_all_locks(&mut self, txn: u32) -> Result<(), SessionError> {
        let file_id = self.opened_file_id()?;
        let request = write::KillAllLocks {
            host_file: file_id as i32,
            txn,
            all: false,
        };
        self.perform_on_file(|id, file| write::build_kill_all_locks(id, file, &request))
            .map(|_| ())
    }

    /// Store a binary-data tree (containers.md 2; `payload` its bracketed
    /// slot list) in the library of base table `table` for container field
    /// `field` (op88, 3): the host answers the new member or, for data it
    /// already holds, that member with one more reference (status
    /// `0x1006`). Either way the caller holds one reference to the member.
    /// A payload larger than one request goes in parts of whole chunks
    /// (3.3, network-protocol.md 5.6.6c "MultiUpload"): the first part
    /// starts the transfer, each later one echoes the previous reply's
    /// handles and copy key, and every reply but the last says the upload
    /// continues.
    pub fn upload_binary(
        &mut self,
        table: &[u8],
        field: &[u8],
        payload: Vec<u8>,
    ) -> Result<om_protocol::container::UploadBinaryReply, SessionError> {
        use om_protocol::container;
        let mut resume = Resume::start();
        let (mut start, mut depth) = (0, 0);
        loop {
            let (end, after) = om_protocol::page_cut(&payload, start, PAGE_LIMIT, depth);
            let request = container::UploadBinary {
                payload: payload[start..end].to_vec(),
                resume,
                table: vec![table.to_vec()],
                field: vec![field.to_vec()],
                solved_path: String::new(),
                reserved: [0; 16],
            };
            let result = self
                .perform_on_file(|id, file| container::build_upload_binary(id, file, &request))?;
            let reply = container::parse_upload_binary_reply(&result)
                .map_err(|e| SessionError::Protocol(format!("upload binary data reply: {e}")))?;
            (start, depth) = (end, after);
            let continues = reply.status == container::UPLOAD_CONTINUES;
            match (start < payload.len(), continues) {
                (_, false) if start < payload.len() => {
                    return Err(SessionError::Protocol(format!(
                        "the host ended a container upload after {start} of {} bytes \
                         (status {:#x})",
                        payload.len(),
                        reply.status
                    )))
                }
                (_, false) => return Ok(reply),
                (false, true) => {
                    return Err(SessionError::Protocol(
                        "the host asked for more of a container upload that was complete".into(),
                    ))
                }
                (true, true) => {}
            }
            if !reply.resume.has_more() {
                return Err(SessionError::Protocol(
                    "the host gave no handle to continue a container upload".into(),
                ));
            }
            resume = Resume {
                depth: -1,
                ..reply.resume
            };
        }
    }

    /// Add `delta` to the reference count of the library member at `path`
    /// (`[T, 1f, 05, M]`; op87, containers.md 5.3) and return the count
    /// after it; [`om_protocol::container::COUNT_FAILED`] when the host has
    /// no such member.
    pub fn adjust_reference_count(
        &mut self,
        path: Vec<Vec<u8>>,
        delta: i32,
    ) -> Result<u32, SessionError> {
        use om_protocol::container;
        let request = container::AdjustReferenceCount { path, delta };
        let result = self.perform_on_file(|id, file| {
            container::build_adjust_reference_count(id, file, &request)
        })?;
        container::parse_adjust_reference_count_reply(&result)
            .map_err(|e| SessionError::Protocol(format!("adjust reference count reply: {e}")))
    }

    /// Perform Script on Server (op30, script-runtime.md 11): run script
    /// `script` (its id in the opened file's script catalog) on the host
    /// with `parameter` as `Get ( ScriptParameter )`, in a session of its
    /// own with this session's account.
    ///
    /// With `wait`, this blocks until the script ends, however long it
    /// runs, and returns its Exit Script result and final `Get ( LastError )`;
    /// the changes it made arrive with the reply (see
    /// [`Session::take_notifications`]). Without, the host answers at once:
    /// error 0, or its refusal (812 when it runs too many scripts), and no
    /// result. A parameter over 1,000,000 characters is error 513 without
    /// asking the host (11.2).
    pub fn perform_script_on_server(
        &mut self,
        script: u64,
        parameter: &str,
        wait: bool,
    ) -> Result<PerformScriptReply, SessionError> {
        self.perform_script(
            script,
            parameter,
            if wait { 0 } else { script::ASYNC },
            CallbackIds::default(),
        )
    }

    /// Perform Script on Server with Callback (11.4): start script `script`
    /// on the host without waiting; when it ends the host sends its result
    /// with `callback`, which [`Session::poll`] and
    /// [`Session::take_psos_results`] deliver. The reply is the host's
    /// answer to starting it (0, or a refusal such as 812). Needs a server
    /// of protocol version 9 or later (error 3 otherwise).
    pub fn perform_script_on_server_with_callback(
        &mut self,
        script: u64,
        parameter: &str,
        callback: u32,
    ) -> Result<PerformScriptReply, SessionError> {
        if self.protocol_version.unwrap_or(0) < script::CALLBACK_IDS_VERSION {
            return Ok(PerformScriptReply {
                error: UNAVAILABLE,
                ..PerformScriptReply::default()
            });
        }
        let file = self.opened_file_id()?;
        let key = self.key;
        let sent = self.link.lock().inbox.register_callback(key, callback);
        let started = self.perform_script(
            script,
            parameter,
            script::ASYNC,
            CallbackIds {
                file_user: file,
                client: sent,
            },
        );
        // A call that did not start sends no result.
        if !matches!(&started, Ok(reply) if reply.error == 0) {
            self.link.lock().inbox.callbacks.remove(&sent);
        }
        started
    }

    fn perform_script(
        &mut self,
        script: u64,
        parameter: &str,
        flags: u16,
        ids: CallbackIds,
    ) -> Result<PerformScriptReply, SessionError> {
        if parameter.chars().count() > script::TEXT_LIMIT {
            return Ok(PerformScriptReply {
                error: script::PARAMETER_TOO_LONG,
                ..PerformScriptReply::default()
            });
        }
        let key = script::script_key(script)
            .ok_or_else(|| SessionError::Protocol(format!("script id {script} has no key")))?;
        let request = PerformScript {
            flags,
            script: key,
            parameter: parameter.to_string(),
            callback: (self.protocol_version.unwrap_or(0) >= script::CALLBACK_IDS_VERSION)
                .then_some(ids),
        };
        let file_id = self.opened_file_id()?;
        let transaction =
            script::build_perform_script(self.take_file_transaction(), file_id, &request);
        let connection_id = self.connection_id;
        // A script runs as long as it needs; only a waiting call blocks on it.
        // A script on the host can change any record.
        let msg = self.call(
            |id| build_perform(id, connection_id, b"", &transaction),
            request.waits(),
            Carries::Records,
        )?;
        let perform = om_protocol::parse_perform_reply(&msg)
            .map_err(|e| SessionError::Protocol(format!("perform script reply: {e}")))?;
        if let Some(notifications) = write::parse_notification_backchannel(&perform.backchannel) {
            self.link.lock().inbox.receive(notifications);
        }
        let mut reply = script::parse_perform_script_reply(&perform.result)
            .map_err(|e| SessionError::Protocol(format!("perform script reply: {e}")))?;
        if !request.waits() {
            // The client discards the result of a call that did not wait.
            reply.result.clear();
        }
        Ok(reply)
    }

    /// Read what the server sends on its own for up to `timeout` (callback
    /// results, notification calls), answering each call. Returns as soon
    /// as a callback result is waiting, with the number waiting. A zero
    /// `timeout` still reads what has already arrived.
    pub fn poll(&mut self, timeout: Duration) -> Result<usize, SessionError> {
        let deadline = Instant::now() + timeout;
        let mut wire = self.link.lock();
        let polled = wire.poll(deadline, self.key);
        polled.map_err(|e| wire.check(e))
    }

    /// The Perform Script on Server with Callback results received so far
    /// for this handle's callback ids, oldest first.
    pub fn take_psos_results(&mut self) -> Vec<PsosResult> {
        let key = self.key;
        let mut wire = self.link.lock();
        let inbox = &mut wire.inbox;
        let (mine, rest): (VecDeque<_>, VecDeque<_>) = std::mem::take(&mut inbox.psos_results)
            .into_iter()
            .partition(|r| inbox.result_handle(r.callback) == Some(key));
        inbox.psos_results = rest;
        // Each with the caller's own id for it.
        mine.into_iter()
            .map(|mut result| {
                if let Some((_, own)) = inbox.callbacks.remove(&result.callback) {
                    result.callback = own;
                }
                result
            })
            .collect()
    }

    /// The change notifications received with Perform Script on Server
    /// replies, in `Notify` calls and in Tickle replies since the last call
    /// (write-path.md 6).
    pub fn take_notifications(&mut self) -> Vec<write::Notification> {
        let key = self.key;
        let mut wire = self.link.lock();
        let inbox = &mut wire.inbox;
        let mut mine = Vec::new();
        for (_, notification, takers) in &mut inbox.notifications {
            if let Some(at) = takers.iter().position(|taker| *taker == key) {
                takers.remove(at);
                mine.push(notification.clone());
            }
        }
        inbox
            .notifications
            .retain(|(_, _, takers)| !takers.is_empty());
        mine
    }

    /// The administrator's messages for this handle received since the
    /// last call, oldest first (network-protocol.md 6.5): Send Message, the
    /// requests to close a file or every file, and the notice that the host
    /// disconnects a file, each naming a file going to the handles holding
    /// it ([`Wire::route_admin`]). FileMaker Pro shows them in dialogs; this
    /// session only keeps them (the latest 64), and lets the caller answer
    /// a disconnect itself: it does not let go of the file, as
    /// [`Self::try_take_admin_messages`] and the session's requests do.
    pub fn take_admin_messages(&mut self) -> Vec<AdminMessage> {
        let key = self.key;
        let mut wire = self.link.lock();
        wire.route_admin(false);
        wire.inbox.take_admin(key)
    }

    /// The host's calls that have arrived read without waiting
    /// ([`Self::poll`] with no timeout), then [`Self::take_admin_messages`],
    /// unless another exchange holds the connection now (a Tickle waiting
    /// for its reply): then `None`, at once. For callers that poll while
    /// idle and must not wait behind a slow host. A read that fails marks
    /// the connection lost ([`Self::connection_lost`]).
    pub fn try_take_admin_messages(&mut self) -> Option<Vec<AdminMessage>> {
        let key = self.key;
        let mut wire = self.link.try_lock()?;
        if let Err(error) = wire.poll_idle() {
            let _ = wire.check(error);
        }
        wire.route_admin(true);
        Some(wire.inbox.take_admin(key))
    }

    /// Issue a raw `Perform` with a prebuilt transaction and return the result stack.
    pub fn perform(&mut self, transaction: &[u8]) -> Result<Vec<u8>, SessionError> {
        Ok(self.perform_reply(transaction)?.result)
    }

    /// [`Session::perform`], returning the backchannel too (where the server
    /// delivers change notifications).
    pub fn perform_reply(&mut self, transaction: &[u8]) -> Result<PerformReply, SessionError> {
        let connection_id = self.connection_id;
        let msg = self.call(
            |id| build_perform(id, connection_id, b"", transaction),
            false,
            carries(transaction),
        )?;
        let reply = om_protocol::parse_perform_reply(&msg)
            .map_err(|e| SessionError::Protocol(format!("perform reply: {e}")))?;
        if let Some(notifications) = write::parse_notification_backchannel(&reply.backchannel) {
            self.link.lock().inbox.receive(notifications);
        }
        Ok(reply)
    }

    /// [`Session::perform`] with an explicit `maxreturn` (the reply size limit).
    pub fn perform_with_max(
        &mut self,
        transaction: &[u8],
        max_return: u32,
    ) -> Result<Vec<u8>, SessionError> {
        let connection_id = self.connection_id;
        let msg = self.call(
            |id| build_perform_with_max(id, connection_id, b"", transaction, max_return),
            false,
            carries(transaction),
        )?;
        let perform = om_protocol::parse_perform_reply(&msg)
            .map_err(|e| SessionError::Protocol(format!("perform reply: {e}")))?;
        if let Some(notifications) = write::parse_notification_backchannel(&perform.backchannel) {
            self.link.lock().inbox.receive(notifications);
        }
        Ok(perform.result)
    }

    /// Disconnect cleanly (`Disconnect(connection_id, "")`). The keepalive
    /// stops first, so no Tickle follows. A lost connection has nothing to
    /// disconnect: the error says so. While other handles share the
    /// connection, only this handle's file closes ([`Session::close_file`]).
    pub fn disconnect(mut self) -> Result<(), SessionError> {
        if self.link.lock().handles > 1 {
            return match self.opened {
                Some(_) => self.close_file(),
                None => Ok(()),
            };
        }
        let Some(mut transport) = self.stop() else {
            return Err(match self.connection_lost() {
                Some(reason) => SessionError::ConnectionLost(reason),
                None => SessionError::Protocol("the session is closed".into()),
            });
        };
        let id = self.link.lock().take_request_id();
        transport.send(&om_protocol::build_disconnect(
            id,
            self.connection_id,
            false,
        ))?;
        transport.close()?;
        Ok(())
    }
}

impl Drop for Session {
    /// While other handles share the connection, close this handle's file
    /// (one op62 Unlock, as FileMaker Pro closes a file). The last handle
    /// stops the keepalive and drops the connection (without a
    /// `Disconnect`; see [`Session::disconnect`]).
    fn drop(&mut self) {
        let others = self.link.lock().handles > 1;
        if others && self.opened.is_some() && self.connection_lost().is_none() {
            let _ = self.close_file();
        }
        self.forget_file();
        let last = {
            let mut wire = self.link.lock();
            wire.handles = wire.handles.saturating_sub(1);
            wire.handles == 0
        };
        if last {
            self.stop();
        }
    }
}

impl Wire {
    /// File `name` ([`held_name`]), if handle `key` holds it.
    fn held(&mut self, key: u64, name: &str) -> Option<&mut HeldFile> {
        self.files
            .iter_mut()
            .find(|file| file.name == name && file.holders.contains(&key))
    }

    /// The transport, unless the connection is lost or closed.
    fn transport(&mut self) -> Result<&mut Transport, SessionError> {
        if let Some(reason) = self.lost.get() {
            return Err(SessionError::ConnectionLost(reason.clone()));
        }
        self.transport
            .as_mut()
            .ok_or_else(|| SessionError::Protocol("the session is closed".into()))
    }

    fn take_request_id(&mut self) -> u32 {
        let id = self.next_request_id;
        self.next_request_id += 2;
        id
    }

    fn take_file_transaction(&mut self) -> u32 {
        let id = self.next_file_transaction;
        self.next_file_transaction = self.next_file_transaction.wrapping_add(1);
        id
    }

    /// Route the administrator's messages that came to the handles that
    /// take them ([`Inbox::admin_routed`]). A notice naming a file (a
    /// request to close it, the host's disconnect of it) goes to the
    /// handles holding that file, by the host's file id with
    /// [`notify::attention::HOST_SERIAL`] in its code and by the client file
    /// id its op63 registered without; one naming a file the connection no
    /// longer holds is dropped (network-protocol.md 6.5). Send Message and
    /// the request to close every file are the connection's.
    ///
    /// With `release`, when the host disconnected a file (`0x0870`), the
    /// connection lets go of it at once, once for all its holders, related
    /// readers included ([`Self::release_forced`]): the host drops a
    /// connection that still holds such a file after its wait for the client
    /// to sign out, and with it the connection's other files. Without, the
    /// caller answers the notice itself ([`Session::take_admin_messages`]).
    fn route_admin(&mut self, release: bool) {
        while let Some(message) = self.inbox.admin_messages.pop_front() {
            let kind = message.code & !notify::attention::HOST_SERIAL;
            let takers = if matches!(
                kind,
                notify::attention::CLOSE_FILE | notify::attention::DISCONNECT_ALL
            ) {
                let by_host = message.code & notify::attention::HOST_SERIAL != 0;
                let named = self
                    .files
                    .iter()
                    .find(|held| {
                        if by_host {
                            held.opened.as_ref().is_some_and(|opened| {
                                i64::from(opened.reply.file_id) == i64::from(message.file)
                            })
                        } else {
                            held.client_file == Some(message.file)
                        }
                    })
                    .or_else(|| {
                        // A client file id the connection never recorded (an
                        // op63 sent as a raw Perform) names the one file it
                        // holds, unless it is the id of a file closed since.
                        // Host ids are recorded at the Open.
                        (!by_host
                            && self.files.len() == 1
                            && !self.inbox.retired.contains(&message.file))
                        .then(|| &self.files[0])
                    });
                let Some(named) = named else {
                    continue;
                };
                let holders = named.holders.clone();
                let host_file = named.opened.as_ref().map(|opened| opened.reply.file_id);
                if release && kind == notify::attention::DISCONNECT_ALL {
                    if let Some(file_id) = host_file {
                        self.release_forced(file_id);
                    }
                }
                Some(holders)
            } else {
                None
            };
            if self.inbox.admin_routed.len() == MAX_ADMIN_MESSAGES {
                self.inbox.admin_routed.pop_front();
            }
            self.inbox.admin_routed.push_back((message, takers));
        }
    }

    /// Let go of host file `file_id`, which the host disconnected: it is
    /// [`Self::revoked`] for every handle holding it, and one op62 closes
    /// its root, as a user's close does (FileMaker Pro signs out with op81
    /// Logout instead, which OpenMaker does not build). Whatever the host
    /// answers, the file is gone here; a transport failure loses the
    /// connection as any exchange's does.
    fn release_forced(&mut self, file_id: u32) {
        if !self.revoked.insert(file_id) {
            return;
        }
        let client = self
            .files
            .iter()
            .find(|held| held.opened.as_ref().map(|opened| opened.reply.file_id) == Some(file_id))
            .and_then(|held| held.client_file);
        self.files.retain(|held| {
            held.opened.as_ref().map(|opened| opened.reply.file_id) != Some(file_id)
        });
        // What still names the file is dropped (`Inbox::receive`).
        if let Some(client) = client {
            self.inbox.retired.insert(client);
        }
        let connection_id = self.connection_id;
        let request = write::Unlock::close_file(file_id as i32);
        let transaction = write::build_unlock(self.take_file_transaction(), file_id, &request);
        let _ = self.call(
            |id| build_perform(id, connection_id, b"", &transaction),
            Wait::Read,
            carries(&transaction),
        );
    }

    /// The Tickle's records (network-protocol.md 6.1): one, session 1,
    /// while any file is open on the connection, however many are. With
    /// one file open that is what FileMaker Pro sends; with several it is
    /// not recorded, and this is the one place to change for a record per
    /// file.
    fn tickle_sessions(&self) -> Vec<TickleSession> {
        if self.inbox.open.is_empty() {
            return Vec::new();
        }
        vec![TickleSession {
            session_id: FILE_SESSION_ID,
            idle_seconds: u32::try_from(self.last_used.elapsed().as_secs()).unwrap_or(u32::MAX),
        }]
    }

    /// Send the request `build` makes with the next request id and return
    /// the reply's message, waiting as `wait` says. A failure that ends the
    /// connection marks it lost.
    fn call(
        &mut self,
        build: impl FnOnce(u32) -> Vec<u8>,
        wait: Wait,
        carries: Carries,
    ) -> Result<Vec<u8>, SessionError> {
        let exchanged = self.exchange(build, wait, carries);
        exchanged.map_err(|e| self.check(e))
    }

    /// [`Wire::call`], leaving a failure as it is: for the handshake, which
    /// has no session to lose yet.
    fn exchange(
        &mut self,
        build: impl FnOnce(u32) -> Vec<u8>,
        wait: Wait,
        carries: Carries,
    ) -> Result<Vec<u8>, SessionError> {
        self.transport()?;
        if carries == Carries::Records {
            add_record_requests_sent(1);
        }
        let id = self.take_request_id();
        let Wire {
            transport, inbox, ..
        } = self;
        let transport = transport.as_mut().expect("checked above");
        let exchanged = transport
            .send(&build(id))
            .map_err(SessionError::from)
            .and_then(|()| recv_reply_message(transport, id, inbox, wait));
        self.last_request = Instant::now();
        exchanged
    }

    /// Read what the server sends on its own until a callback result for
    /// `handle` is waiting or `deadline` passes (see [`Session::poll`]).
    fn poll(&mut self, deadline: Instant, handle: u64) -> Result<usize, SessionError> {
        self.transport()?;
        let Wire {
            transport, inbox, ..
        } = self;
        let transport = transport.as_mut().expect("checked above");
        let polled = (|| {
            while inbox.results_for(handle) == 0 {
                let left = deadline.saturating_duration_since(Instant::now());
                transport.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
                let msg = match transport.recv_logical() {
                    Ok(msg) => msg,
                    Err(e) if e.is_timeout() => break,
                    Err(e) => return Err(e.into()),
                };
                handle_unsolicited(transport, &msg, inbox)?;
            }
            Ok(inbox.results_for(handle))
        })();
        transport.set_read_timeout(Some(READ_TIMEOUT))?;
        polled
    }

    /// [`Self::poll`] for a caller that polls while idle and must not be
    /// held: at most [`IDLE_MESSAGES`] messages, each read by
    /// [`IDLE_READ`] from its start however slowly it arrives (what is
    /// read of it is kept for the next read), and the answers to the host's
    /// calls written within [`IDLE_WRITE_TIMEOUT`] of the poll's start, all
    /// of them: an answer not taken by then fails the write, which loses the
    /// connection. Messages left are read by the next poll or exchange.
    fn poll_idle(&mut self) -> Result<(), SessionError> {
        self.transport()?;
        let Wire {
            transport, inbox, ..
        } = self;
        let transport = transport.as_mut().expect("checked above");
        transport.set_write_deadline(Some(Instant::now() + IDLE_WRITE_TIMEOUT))?;
        let polled = (|| {
            for _ in 0..IDLE_MESSAGES {
                let msg = match transport
                    .recv_logical_by(Instant::now() + IDLE_READ, Some(READ_TIMEOUT))
                {
                    Ok(msg) => msg,
                    Err(e) if e.is_timeout() => break,
                    Err(e) => return Err(e.into()),
                };
                match handle_unsolicited(transport, &msg, inbox) {
                    Ok(()) => {}
                    // An answer half written: the stream cannot go on.
                    Err(SessionError::Transport(e)) if e.is_timeout() => {
                        return Err(SessionError::ConnectionLost(
                            "the host stopped taking the client's answers".into(),
                        ))
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        })();
        transport.set_write_deadline(None)?;
        polled
    }

    /// A failure of the transport (other than a read that timed out) or a
    /// connection the host closed loses the connection: the socket is
    /// dropped and every later request fails with the same reason.
    fn check(&mut self, error: SessionError) -> SessionError {
        let reason = match &error {
            SessionError::Transport(e) if !e.is_timeout() => e.to_string(),
            SessionError::ConnectionLost(reason) => reason.clone(),
            _ => return error,
        };
        self.lose(reason)
    }

    fn lose(&mut self, reason: String) -> SessionError {
        let reason = self.lost.get_or_init(|| reason).clone();
        self.transport = None;
        SessionError::ConnectionLost(reason)
    }

    /// One Tickle reporting the open file's idle time (network-protocol.md
    /// 6.1). A host that has not answered within the Tickle timeout (from
    /// sending it, whatever else it sent meanwhile) is taken to be gone, as
    /// FileMaker Pro's 30-second call timeout around Tickle does; anything
    /// it answers, or a refusal, keeps the connection.
    fn tickle(&mut self) -> Result<(), SessionError> {
        let sessions = self.tickle_sessions();
        let connection_id = self.connection_id;
        let wait = Wait::Until(Instant::now() + self.tickle_timeout);
        let tickle = |id| build_tickle(id, connection_id, b"", &sessions);
        let msg = match self.call(tickle, wait, Carries::NoRecords) {
            Ok(msg) => msg,
            Err(SessionError::Transport(e)) if e.is_timeout() => {
                return Err(self.lose("the host stopped answering".into()));
            }
            Err(e) => return Err(e),
        };
        let parms = parse_tickle_reply(&msg)
            .map_err(|e| SessionError::Protocol(format!("tickle reply: {e}")))?;
        if let Some(notifications) = write::parse_notification_backchannel(&parms) {
            self.inbox.receive(notifications);
        }
        Ok(())
    }
}

/// The keepalive thread: a Tickle whenever the connection has been silent
/// for the interval, until the session ends or the connection is lost.
fn keepalive(link: &Link) {
    let mut wire = link.lock();
    loop {
        if wire.closing || wire.lost.get().is_some() || wire.transport.is_none() {
            return;
        }
        let Some(interval) = wire.keepalive else {
            wire = link.wake.wait(wire).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let left = (wire.last_request + interval).saturating_duration_since(Instant::now());
        if left.is_zero() {
            // A refused Tickle still kept the connection busy; a lost
            // connection ends the loop above.
            let _ = wire.tickle();
            // A file the host disconnected meanwhile is let go of now, also
            // while the client waits in a script's pause.
            wire.route_admin(true);
            continue;
        }
        wire = link
            .wake
            .wait_timeout(wire, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// Receive the reply to `request_id`, returning its raw message bytes.
///
/// Reassembles GIOP fragments and answers the server's calls that may arrive
/// before our reply, waiting as `wait` says: [`Wait::Patient`] keeps waiting
/// past the transport's read timeout (a Perform Script on Server that waits
/// for its script); [`Wait::Until`] gives up at its deadline, a timeout,
/// even while the server keeps sending other messages.
fn recv_reply_message(
    transport: &mut Transport,
    request_id: u32,
    inbox: &mut Inbox,
    wait: Wait,
) -> Result<Vec<u8>, SessionError> {
    loop {
        let read = match wait {
            Wait::Until(deadline) => transport.recv_logical_by(deadline, Some(READ_TIMEOUT)),
            Wait::Read | Wait::Patient => transport.recv_logical(),
        };
        let msg = match read {
            Ok(msg) => msg,
            Err(e) if matches!(wait, Wait::Patient) && e.is_timeout() => continue,
            Err(e) => return Err(e.into()),
        };
        let (header, _) = parse_header(&msg).map_err(|e| SessionError::Protocol(format!("{e}")))?;
        if header.msg_type == MsgType::Reply {
            let reply = Reply::parse(&msg).map_err(|e| SessionError::Protocol(format!("{e}")))?;
            if reply.request_id == request_id {
                return Ok(msg);
            }
            // A reply for a different id (unexpected in our serial flow): skip.
            continue;
        }
        handle_unsolicited(transport, &msg, inbox)?;
    }
}

/// A message that is not the reply being waited for: answer a call of the
/// server, skip a stray reply, fail on anything else.
fn handle_unsolicited(
    transport: &mut Transport,
    msg: &[u8],
    inbox: &mut Inbox,
) -> Result<(), SessionError> {
    let (header, _) = parse_header(msg).map_err(|e| SessionError::Protocol(format!("{e}")))?;
    match header.msg_type {
        MsgType::Request => answer_backchannel(transport, msg, inbox),
        MsgType::Reply => Ok(()),
        MsgType::LocateRequest => {
            // FileMaker Server locates the callback object before calling it
            // (write-path.md 6.4).
            if let Ok(id) = om_giop::parse_locate_request(msg) {
                transport.send(&om_giop::build_locate_reply_here(id))?;
            }
            Ok(())
        }
        MsgType::CloseConnection => Err(SessionError::ConnectionLost(
            "the host closed the connection".into(),
        )),
        other => Err(SessionError::Protocol(format!(
            "unexpected message {other:?}"
        ))),
    }
}

/// Answer a call the server makes on the client's callback object.
///
/// - `Perform` with op105, a Perform Script on Server with Callback result
///   (script-runtime.md 11.4), is kept and answered with status 0
///   (inferred); other opcodes get error 3.
/// - `Notify` carries change notifications and the administrator's messages
///   in `parms` (write-path.md 6.4, network-protocol.md 6.5); they are kept
///   and the call is answered NO_EXCEPTION with an empty `backchannel`, as
///   FileMaker Pro answers before it shows anything.
/// - `Disconnect` ends the session: the host disconnects a client it asked
///   to close its files once they are closed (network-protocol.md 6.5). It
///   is answered, and the connection is then lost.
/// - Anything else (`Tickle`) gets an empty NO_EXCEPTION reply.
fn answer_backchannel(
    transport: &mut Transport,
    msg: &[u8],
    inbox: &mut Inbox,
) -> Result<(), SessionError> {
    let req = Request::parse(msg).map_err(|e| SessionError::Protocol(format!("{e}")))?;
    let reply = match req.operation {
        op::PERFORM => {
            let transaction = parse_perform_request(req.body).map(|p| p.transaction);
            let result = match transaction {
                Some(txn) if txn.first() == Some(&(Opcode::SendPsosResultToClient as u8)) => {
                    match script::parse_psos_result(txn) {
                        Ok(result) => {
                            inbox.receive_result(result);
                            script::psos_result_reply()
                        }
                        Err(_) => ResultBuilder::new().push_u16(UNAVAILABLE).finish(),
                    }
                }
                _ => ResultBuilder::new().push_u16(UNAVAILABLE).finish(),
            };
            build_perform_reply(req.request_id, &result, &[])
        }
        op::NOTIFY => {
            if let Some(notifications) = notify::parse_notify_request(req.body)
                .and_then(|call| write::parse_notification_backchannel(call.parms))
            {
                inbox.receive(notifications);
            }
            notify::build_notify_reply(req.request_id, &[])
        }
        op::DISCONNECT => {
            if req.response_expected {
                transport.send(&build_reply(
                    req.request_id,
                    ReplyStatus::NoException,
                    &[],
                    &[],
                ))?;
            }
            return Err(SessionError::ConnectionLost(
                "the host ended the session".into(),
            ));
        }
        _ => build_reply(req.request_id, ReplyStatus::NoException, &[], &[]),
    };
    if req.response_expected {
        transport.send(&reply)?;
    }
    Ok(())
}

/// An op14 or op70 reply must be the complete single-part page (spec 3.3):
/// this client never uploads in parts.
fn complete_upload(result: &[u8], what: &str) -> Result<(), SessionError> {
    let resume = write::parse_upload_reply(result)
        .map_err(|e| SessionError::Protocol(format!("{what} reply: {e}")))?;
    if resume.has_more() {
        return Err(SessionError::Protocol(format!(
            "server asked to continue a single-part {what}"
        )));
    }
    Ok(())
}

/// A page that asks to be continued must carry data, or a misbehaving server
/// could keep the client looping forever.
fn check_progress(resume: &Resume, page_len: usize) -> Result<(), SessionError> {
    if resume.has_more() && page_len == 0 {
        return Err(SessionError::Protocol(
            "server asked to continue a transfer without sending data".into(),
        ));
    }
    Ok(())
}
