//! One client connection: reads GIOP requests, answers them, and keeps the
//! session state (connection id and key, open files, logins).
//!
//! A session ends at `Disconnect`, at a new `Connect`, when the server calls
//! `Disconnect` on a client that closed its files when asked to
//! disconnect, and when its connection is dropped: whenever its thread
//! leaves `Connection::serve`, because the client left, the administrator
//! disconnected it, it stayed silent too long, more change notifications
//! waited for it than the server keeps (`writes::Mailboxes`), a write
//! failed, or a request handler panicked. Its end releases everything the
//! session holds in shared state (`Connection::end_session`), which
//! therefore never panics, and takes its locks even when a panic poisoned
//! them.
//!
//! The handlers mirror what FileMaker Server returns for each operation (spec
//! `docs/re/network-protocol.md` sections 3 and 5). Where FileMaker's choice is
//! not observed (error codes for refusals), the choice is marked as OpenMaker's.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime};

use log::{debug, error, info, warn};
use om_giop::{
    build_reply, build_system_exception_reply, parse_header, Completion, MessageReader, MsgType,
    ReadError, ReplyStatus, Request,
};
use om_hbam::OwnedNode;
use om_protocol::container::{self, UploadBinaryReply};
use om_protocol::hosted_files;
use om_protocol::notify;
use om_protocol::schema;
use om_protocol::script;
use om_protocol::write;
use om_protocol::{
    build_connect_reply, build_is_a_reply, build_perform_reply, build_tickle_reply, op,
    parse_connect_request, parse_disconnect_request, parse_download_list_request,
    parse_download_request, parse_file_list_request, parse_is_a_request, parse_login_request,
    parse_open_request, parse_perform_request, parse_query_request, parse_request_notification,
    parse_tickle_parms, parse_tickle_request, ConnectReply, DownloadListReply, DownloadReply,
    FileListReply, KeyList, LoginReply, Opcode, OpenReply, PerformError, QueryReply,
    ReceivedDownload, Resume, OBJECT_KEY, REPO_ID,
};
use om_rctransaction::{Header, ResultBuilder};
use ring::rand::{SecureRandom, SystemRandom};

use crate::admin::{Clients, Control, Farewell, FileStatus};
use crate::authoring;
use crate::config::{ConnectionLimits, ServerIdentity};
use crate::containers;
use crate::engine::{
    AdvanceSerial, CommitRecords, Draft, FileEngine, Operation, ReleaseLock, ReleaseLocks, Reply,
    ReserveRecordIds, TakeLock, UpdateTable, UpgradeLock, UploadToCatalog,
};
use crate::eventlog::{ClientLabel, Event, EventLog, SessionEnd};
use crate::hosting::{Eligibility, FileAccount, Hosted, Hosting, Registration};
use crate::locks::LockUser;
use crate::login_limits::{Attempt, LoginLimits, LOGIN_TOO_MANY_ATTEMPTS};
use crate::record::{Recorder, Recordings};
use crate::scripts::{self, ScriptEngine};
use crate::transfer::{
    page_limit, DownloadMemory, Owner, Refused, Transfers, Upload, MAX_PRE_LOGIN_UPLOAD, MAX_UPLOAD,
};
use crate::uploads::{self, Charge, UploadMemory};
use crate::writes::{self, Mailboxes};
use om_data::host::security::{RecordAction, SetAccess};
use om_query::Context as QueryContext;

/// Largest GIOP message accepted from a client: the largest `Perform` request
/// (`0x200000`, spec 5.6.2) plus framing.
const MAX_REQUEST_LEN: usize = 0x20_0000 + 0x1000;

/// `maxreturn` bounds FileMaker Server enforces (spec 5.6.2).
const MAX_RETURN_RANGE: std::ops::RangeInclusive<u32> = 0x14..=0x20_0000;

/// The lowest client protocol version FileMaker Server accepts is above this.
const MIN_PROTOCOL_EXCLUSIVE: u8 = 0x1a;

/// The out `backchannel` of every `Perform` reply: `0xFE` (server CPU
/// report) and a load byte (spec 5.6.3b).
const BACKCHANNEL: [u8; 2] = [0xfe, 0x00];

/// The protocol version a successful Login reply names (spec 5.6.5), which
/// fixes the layout of later requests such as op30's callback ids.
pub(crate) const PROTOCOL_VERSION: u16 = 9;

/// The first request id of the calls the server makes on the client's
/// callback object: GIOP 1.2 gives the side that accepted a bidirectional
/// connection the odd ids, and FileMaker Server's first is 3 (spec 6.5).
const FIRST_CALL_ID: u32 = 3;

/// Open (op78) status for a file that is not hosted (spec 5.5).
const OPEN_FILE_MISSING: u16 = 4;

/// Open (op78) status under Block New Users: FileMaker's error 1638, "The
/// host is not allowing new connections" (spec 5.6.4, 6.5).
const OPEN_BLOCKED: u16 = 1638;

/// Status for an operation OpenMaker Server does not implement yet
/// (FileMaker error 3, "command is unavailable"; OpenMaker's choice).
const UNSUPPORTED: u16 = 3;

/// Login error for a wrong account or password: FileMaker error 212, as
/// FileMaker Server answered a wrong password (network-protocol.md 5.6.5).
const LOGIN_REJECTED: u16 = 212;

/// Login errors for a correct password of an account the host does not
/// admit (`hosting::Eligibility`): FileMaker's 210 (user account is
/// inactive) and 211 (password has expired, so it must be changed first),
/// as FileMaker Pro refuses such an account of a file it opens
/// (docs/re/local-files.md 2.2); and OpenMaker's choices 3 (command is
/// unavailable) for account settings in a form it cannot read and 9
/// (insufficient privileges) for a privilege set it cannot enforce.
pub const LOGIN_INACTIVE_ACCOUNT: u16 = 210;
pub const LOGIN_PASSWORD_CHANGE_DUE: u16 = 211;
pub const LOGIN_UNREADABLE_ACCOUNT: u16 = 3;
pub const LOGIN_UNSUPPORTED_PRIVILEGES: u16 = 9;

/// The CORBA system exception for a request refused at a limit on what one
/// connection holds (transfer.rs): the client reports an error instead of
/// reading a result. OpenMaker's choice; FileMaker Server's limits are
/// unmeasured.
const LIMIT_EXCEPTION: &str = "IMP_LIMIT";

/// Download lock error for a key the session may not read yet (FileMaker
/// error 200, "record access is denied"; OpenMaker's choice).
pub(crate) const ACCESS_DENIED: u16 = 200;
/// FileMaker's error for a file id not open on the connection
/// (`kFileMissing`, containers.md 3.2).
const FILE_NOT_OPEN: u16 = 100;

/// The parts of a file readable before Login, as FileMaker Server allows:
/// the status area `[2]` (the client needs `mInternalCreation` from it to log
/// in, spec 5.4) and `[23, 1, 1]` in the security catalog (key 23 = 0x17),
/// which FileMaker Pro reads before Login and needs to show any layout. A
/// download is allowed when its path starts with one of these.
const PRE_LOGIN_PATHS: &[&[&[u8]]] = &[&[&[2]], &[&[23], &[1], &[1]]];

/// A synthetic op90 reply for one path; ordinary storage stays unchanged.
pub(crate) struct DownloadOverride {
    pub path: Vec<Vec<u8>>,
    pub slots: Vec<u8>,
}

/// State shared by every connection of one server.
pub(crate) struct Shared {
    pub identity: ServerIdentity,
    pub hosting: Hosting,
    /// A file hosted privately for a local client ([`crate::LocalHost`]):
    /// its logins are not limited, and deferred work its opening finished
    /// is saved only once an account logs in.
    pub local: bool,
    /// How often logins may fail ([`LoginLimits`]; not for `local`).
    pub login_limits: LoginLimits,
    /// Admit accounts in custom privilege sets though this is a network
    /// server ([`crate::Server::admitting_custom_sets`]; a test fixture).
    pub custom_sets: AtomicBool,
    pub rng: SystemRandom,
    /// Connection ids in use, so a new one is never handed out twice.
    pub connection_ids: Mutex<HashSet<u32>>,
    /// Where to record connections, if anywhere.
    pub recordings: Option<Recordings>,
    /// Change notifications waiting for each connection.
    pub mailboxes: Mailboxes,
    /// Runs the operations that change hosted files ([`crate::engine`]).
    pub engine: FileEngine,
    /// Perform Script on Server (script-runtime.md 11).
    pub scripts: ScriptEngine,
    /// Key paths no download may read after Login, with what is under them
    /// (`Server::refusing_downloads`).
    pub refused_downloads: RwLock<Vec<Vec<Vec<u8>>>>,
    /// Paths omitted from op92 copies only, for partial-reply fixtures.
    pub omitted_list_paths: RwLock<Vec<Vec<Vec<u8>>>>,
    pub download_overrides: RwLock<Vec<DownloadOverride>>,
    /// The server is shutting down: stop accepting connections.
    pub shutdown: AtomicBool,
    /// Clients' finds on a missing index get no records, the index being
    /// built afterwards (`Server::with_late_index_builds`).
    pub late_index_builds: AtomicBool,
    /// Omit the QueryProgram root terminator for client error-handling tests.
    pub unterminated_query_replies: AtomicBool,
    /// When the server started.
    pub started: SystemTime,
    /// Block New Users: refuse to open files for sessions that have none
    /// open ([`crate::ServerHandle::set_block_new_users`]).
    pub block_new_users: AtomicBool,
    /// The connected clients, as the administrator sees them.
    pub clients: Clients,
    /// The event log and alerts ([`crate::eventlog`]).
    pub events: EventLog,
    /// Every accepted connection's socket, from its accept (before its TLS
    /// handshake) until its thread ends ([`Sockets`]).
    pub sockets: Sockets,
    /// How long an Open of a file waits before it is answered, by lowercase
    /// file name ([`crate::Server::delaying_opens`]).
    pub open_delays: RwLock<HashMap<String, Duration>>,
    /// The hosted files clients' file list names, by lowercase name
    /// ([`crate::Server::listing_file`]).
    pub listed: RwLock<HashSet<String>>,
    /// The body sent instead of the file list, for tests of clients
    /// ([`crate::Server::overriding_file_list`]).
    pub file_list_override: RwLock<Option<Vec<u8>>>,
    /// Deny having the file list, as a host without it
    /// ([`crate::Server::without_file_list`]).
    pub no_file_list: AtomicBool,
    /// Answer GetFileList (op77) as unsupported (error 3)
    /// ([`crate::Server::without_get_file_list`]).
    pub no_get_file_list: AtomicBool,
    /// Deny having the authoring extension, as a host without it
    /// ([`crate::Server::without_authoring`]).
    pub no_authoring: AtomicBool,
    /// The memory clients' uploads take as trees, on the whole server and
    /// for each client address ([`crate::uploads`]).
    pub upload_memory: Arc<UploadMemory>,
    /// The memory the download streams connections keep paged take, on
    /// the whole server ([`crate::transfer`]).
    pub download_memory: Arc<DownloadMemory>,
    /// A request that panics ([`crate::Server::panicking_on`]).
    #[cfg(debug_assertions)]
    pub test_panic: std::sync::OnceLock<TestPanic>,
    /// An op71 refused ([`crate::Server::refusing_create_after`]).
    #[cfg(debug_assertions)]
    pub test_refusal: std::sync::OnceLock<TestPanic>,
    /// Where Perform Script on Server requests wait between their
    /// admission and their script's start ([`crate::Server::holding_scripts`]).
    #[cfg(debug_assertions)]
    pub script_gate: std::sync::OnceLock<ScriptGate>,
    /// While set, HostTimeStamp fails ([`crate::Server::failing_timestamps`]).
    #[cfg(debug_assertions)]
    pub failing_timestamps: Arc<AtomicBool>,
}

/// The sockets of a server's accepted connections, so that they can be
/// closed whatever their connection is doing ([`crate::LocalHost`] closes
/// them all when it closes its file): a connection waiting in its TLS
/// handshake has no session the administrator could disconnect.
#[derive(Default)]
pub(crate) struct Sockets {
    /// Each socket, with the flag [`Sockets::close_all`] sets before it
    /// closes it, which its connection reads as the server having closed
    /// it.
    open: Mutex<HashMap<u64, (std::net::TcpStream, Arc<AtomicBool>)>>,
    next: std::sync::atomic::AtomicU64,
}

impl Sockets {
    /// Keep a handle of `socket` until the returned id is
    /// [`Sockets::close`]d; the flag is set when [`Sockets::close_all`]
    /// closes it.
    pub fn track(&self, socket: &std::net::TcpStream) -> Option<(u64, Arc<AtomicBool>)> {
        let handle = socket.try_clone().ok()?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let closed = Arc::new(AtomicBool::new(false));
        self.open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, (handle, closed.clone()));
        Some((id, closed))
    }

    /// Shut the socket `id` down both ways and stop keeping it: its
    /// connection ended. The client sees the end of the stream even when
    /// the connection's thread ended by a panic, without a TLS goodbye.
    pub fn close(&self, id: u64) {
        let socket = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        if let Some((socket, _)) = socket {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Shut down every socket, both ways: blocked reads and writes return
    /// and their connections end. Returns how many there were.
    pub fn close_all(&self) -> usize {
        let open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (socket, closed) in open.values() {
            closed.store(true, Ordering::Release);
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        open.len()
    }
}

impl Shared {
    pub(crate) fn random<const N: usize>(&self) -> [u8; N] {
        let mut out = [0u8; N];
        self.rng
            .fill(&mut out)
            .expect("the system random source is available");
        out
    }

    /// The connection ids in use, also after a thread panicked holding
    /// them: each change is one insert or remove, which leaves the set
    /// whole, and a session's end must still release its id.
    fn connection_ids(&self) -> MutexGuard<'_, HashSet<u32>> {
        self.connection_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// A fresh nonzero connection id: for a client connection, or for a
    /// script session's locks and notifications.
    pub(crate) fn new_connection_id(&self) -> u32 {
        let mut ids = self.connection_ids();
        loop {
            let candidate = u32::from_le_bytes(self.random());
            if candidate != 0 && ids.insert(candidate) {
                return candidate;
            }
        }
    }

    pub(crate) fn release_connection_id(&self, id: u32) {
        self.connection_ids().remove(&id);
    }

    /// Whether a connection id is still in use.
    pub(crate) fn connection_alive(&self, id: u32) -> bool {
        self.connection_ids().contains(&id)
    }
}

/// A file opened on this connection.
struct OpenFile {
    hosted: Arc<Hosted>,
    /// The per-file nonce sent in the Open reply (the first Login nonce half).
    nonce: u32,
    /// The file's account this session logged in as, as it was at the
    /// Login. FileMaker keeps a client's privileges until it opens the file
    /// again, whatever changes its account or privilege set meanwhile
    /// (Claris's help, "Managing security"), so a change over the wire, a
    /// deleted account included, counts from the session's next Login.
    account: Option<Arc<FileAccount>>,
    /// How far the administrator's close of the file has gone with this
    /// client (`Connection::file_closes`).
    close: CloseStep,
    /// The records this session was served without their values, which it
    /// may not edit (`views::Withheld`, #670).
    withheld: std::sync::Mutex<crate::views::Withheld>,
}

/// What a client that has a closing file open has been sent
/// (`ServerHandle::close_file`, spec 6.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseStep {
    /// Nothing: the file is not closing, or the connection has not seen it.
    None,
    /// The request to close it.
    Asked,
    /// The request again, halfway through the grace time.
    AskedAgain,
    /// The forced notice, when the grace time ended.
    Forced,
}

/// Requests one connection may hold for paused files, and their bytes; one
/// more is refused with [`LIMIT_EXCEPTION`] (OpenMaker's choice: FileMaker
/// Pro waits for one request at a time and goes on tickling).
const MAX_HELD: usize = 64;
const MAX_HELD_BYTES: usize = 8 << 20;

/// A request held while its file is paused.
struct Held {
    /// The file it names.
    file: u32,
    /// The whole GIOP message.
    msg: Vec<u8>,
}

/// The session established by `Connect`.
struct Session {
    connection_id: u32,
    key: [u8; 16],
    /// The client's machine name, shown to other users as the holder of its
    /// locks.
    name: String,
    open: HashMap<u32, OpenFile>,
    /// Paged downloads and partial uploads in progress (spec 5.6.6c).
    transfers: Transfers,
    /// Records uploaded with op14 and not yet committed, per file
    /// (write-path.md 3.3).
    temp: HashMap<u32, writes::TempArea>,
    /// The address its uploads' memory is charged to
    /// ([`crate::admission::client_key`]).
    client: IpAddr,
    /// The client's version string (Connect's machine info).
    version: String,
    /// The client's address.
    address: IpAddr,
    /// How `Access.log` names the client from its first accepted Login on;
    /// before it, nothing of the session is logged there, as FileMaker
    /// Server logs a connection at its first Login (server-logs.md 3).
    access: Option<ClientLabel>,
    /// Whether the client's callback object has been located for the
    /// session's `Notify` calls.
    callback: Callback,
    /// The perform handle and file count of the op77 list being paged, so
    /// that each later page carries the list's count.
    file_list: Option<(u64, u16)>,
    /// This connection's failed logins with an empty password, FileMaker
    /// Pro's automatic first Login among them, which a later accepted
    /// login on it takes back (`login_limits`, #440); the latest
    /// [`MAX_AUTOMATIC_LOGINS`].
    automatic_logins: Vec<Attempt>,
}

/// The empty-password failures a connection keeps for taking back: one per
/// file it opens before it logs in again. Beyond it the oldest stays
/// counted.
const MAX_AUTOMATIC_LOGINS: usize = 8;

/// The client's callback object, as the session's `Notify` calls need it:
/// FileMaker Server locates it with a GIOP LocateRequest before its first
/// call of a session and calls it once the client answers OBJECT_HERE
/// (spec 6.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Callback {
    /// Not asked yet.
    Unlocated,
    /// The LocateRequest with this id waits for its reply.
    Locating(u32),
    /// The client answered OBJECT_HERE.
    Here,
    /// The client answered something else: it takes no calls.
    Absent,
}

/// One client connection.
pub(crate) struct Connection {
    shared: Arc<Shared>,
    /// The client's address.
    address: SocketAddr,
    peer: String,
    /// What the administrator asks of the connection
    /// ([`crate::ServerHandle::disconnect_client`] and
    /// [`crate::ServerHandle::send_message`]).
    control: Arc<Control>,
    /// Set when the server closed the connection's socket
    /// ([`crate::ServerHandle::close_connections`]).
    closed_by_server: Arc<AtomicBool>,
    session: Option<Session>,
    /// How the session ends, for its last line in `Access.log`.
    ending: SessionEnd,
    recorder: Option<Recorder>,
    /// Requests held while their files are paused, oldest first; at most
    /// [`MAX_HELD`] of [`MAX_HELD_BYTES`] in all.
    held: std::collections::VecDeque<Held>,
    held_bytes: usize,
    /// The request id of the next call on the client's callback object.
    next_call: u32,
}

impl Connection {
    /// A connection from `address`, doing what the administrator asks
    /// through `control` (shared with the connection's
    /// [`crate::admin::ClientSocket`]).
    pub fn new(shared: Arc<Shared>, address: SocketAddr, control: Arc<Control>) -> Self {
        let peer = address.to_string();
        let recorder = shared.recordings.as_ref().and_then(|recordings| {
            match recordings.start(&peer) {
                Ok((index, recorder)) => {
                    info!("{peer}: recording as conn-{index:03}");
                    Some(recorder)
                }
                Err(e) => {
                    error!(
                        "{peer}: NOT RECORDING this connection: cannot create its folder in {}: {e}",
                        recordings.dir().display()
                    );
                    None
                }
            }
        });
        Self {
            shared,
            address,
            peer,
            control,
            closed_by_server: Arc::default(),
            session: None,
            ending: SessionEnd::Closed,
            recorder,
            held: Default::default(),
            held_bytes: 0,
            next_call: FIRST_CALL_ID,
        }
    }

    /// The connection's socket is closed by the server when `closed` is set
    /// ([`Sockets::track`]).
    pub fn closed_by_server(mut self, closed: Arc<AtomicBool>) -> Self {
        self.closed_by_server = closed;
        self
    }

    /// Record a message in one direction; a failed write stops recording.
    fn record(&mut self, from_client: bool, msg: &[u8]) {
        if let Some(recorder) = &mut self.recorder {
            let written = if from_client {
                recorder.client(msg)
            } else {
                recorder.server(msg)
            };
            if let Err(e) = written {
                warn!("{}: recording stopped: {e}", self.peer);
                self.recorder = None;
            }
        }
    }

    /// Answer requests until the client closes the stream, the
    /// administrator disconnects it, it stays silent past its idle limit
    /// in `limits`, or a write fails. A stream with a read timeout lets the
    /// connection send its back-channel calls while the client is quiet,
    /// and notice the administrator's disconnect and the idle limit.
    pub fn serve(mut self, stream: &mut (impl Read + Write), limits: &ConnectionLimits) {
        let mut reader = MessageReader::new(MAX_REQUEST_LEN);
        let mut heard = Instant::now();
        loop {
            let read = reader.read_logical(stream);
            // What the client sent after the administrator's disconnect is
            // not answered.
            if self.control.disconnecting() {
                debug!("{}: closing for the administrator", self.peer);
                break;
            }
            // More notifications wait than the server keeps for one
            // connection (`writes::Mailboxes`): the client learns of it by
            // the connection closing, and reads the files again when it
            // reconnects.
            if self.notifications_overflowed() {
                info!("{}: closing: too many notifications wait for it", self.peer);
                self.ending = SessionEnd::Overflow;
                let close = om_giop::build_close_connection();
                self.record(false, &close);
                let _ = stream.write_all(&close).and_then(|()| stream.flush());
                break;
            }
            let msg = match read {
                Ok(msg) => msg,
                Err(ReadError::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    let limit = self.idle_limit(limits);
                    if let Some(limit) = limit.filter(|limit| heard.elapsed() >= *limit) {
                        // As FileMaker Server closes a silent connection
                        // (network-protocol.md 6.2).
                        let session = if self.session.is_some() {
                            ""
                        } else {
                            " (no session)"
                        };
                        info!(
                            "{}: closing after {limit:?} without a request{session}",
                            self.peer
                        );
                        self.ending = SessionEnd::Idle(limit);
                        let close = om_giop::build_close_connection();
                        self.record(false, &close);
                        let _ = stream.write_all(&close).and_then(|()| stream.flush());
                        break;
                    }
                    if let Err(e) = self.answer_held(stream) {
                        self.write_failed("a reply", &e, limits);
                        break;
                    }
                    if let Err(e) = self.send_calls(stream, limits) {
                        self.write_failed("a back-channel call", &e, limits);
                        break;
                    }
                    continue;
                }
                Err(ReadError::Closed) => {
                    self.ended_by_reading(None);
                    break;
                }
                // Closing TCP without a TLS close_notify is a normal way for a
                // client to leave.
                Err(ReadError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    debug!("{}: closed without close_notify", self.peer);
                    self.ended_by_reading(None);
                    break;
                }
                Err(e) => {
                    warn!("{}: {e}", self.peer);
                    self.ended_by_reading(Some(e.to_string()));
                    break;
                }
            };
            self.record(true, &msg);
            match parse_header(&msg).map(|(h, _)| h.msg_type) {
                Ok(MsgType::Request) => {
                    // Requests held for files resumed since go first.
                    if let Err(e) = self.answer_held(stream) {
                        self.write_failed("a reply", &e, limits);
                        break;
                    }
                    let reply = match self.held_for(&msg) {
                        Some(file) => self.hold(file, &msg),
                        None => self.handle_request(&msg),
                    };
                    if let Some(reply) = reply {
                        self.record(false, &reply);
                        if let Err(e) = stream.write_all(&reply).and_then(|()| stream.flush()) {
                            self.write_failed("a reply", &e, limits);
                            break;
                        }
                    }
                }
                Ok(MsgType::LocateRequest) => {
                    if let Ok(id) = om_giop::parse_locate_request(&msg) {
                        let reply = om_giop::build_locate_reply_here(id);
                        self.record(false, &reply);
                        if let Err(e) = stream.write_all(&reply).and_then(|()| stream.flush()) {
                            self.write_failed("a reply", &e, limits);
                            break;
                        }
                    }
                }
                // The client located its callback object: the session's
                // `Notify` calls waiting for that go out below.
                Ok(MsgType::LocateReply) => self.located(&msg),
                Ok(MsgType::CloseConnection) => break,
                // The client's answer to a back-channel call: nothing waits
                // for it.
                Ok(MsgType::Reply) => self.call_answered(&msg),
                Ok(other) => debug!("{}: ignoring GIOP {other:?}", self.peer),
                Err(e) => {
                    warn!("{}: {e}", self.peer);
                    self.ending = SessionEnd::Lost(e.to_string());
                    break;
                }
            }
            // After every message, whatever it was, as at every read
            // timeout: requests held for files resumed since are answered,
            // the calls waiting go out and the administrator's deadlines
            // advance, so a client that never pauses cannot hold them off.
            if let Err(e) = self.answer_held(stream) {
                self.write_failed("a reply", &e, limits);
                break;
            }
            if let Err(e) = self.send_calls(stream, limits) {
                self.write_failed("a back-channel call", &e, limits);
                break;
            }
            // The client's allowance starts again once its request is
            // answered: the time spent answering it is not the client's
            // silence.
            heard = Instant::now();
        }
        // Dropping the connection here ends its session (its `Drop`).
    }

    /// The connection ended at a read: the server closed its socket
    /// ([`crate::ServerHandle::close_connections`]), or else the client
    /// left, or reading failed with `failure`.
    fn ended_by_reading(&mut self, failure: Option<String>) {
        self.ending = if self.closed_by_server.load(Ordering::Acquire) {
            SessionEnd::ClosedByServer
        } else if self.control.disconnecting() {
            // ClientSocket returns ConnectionAborted for this connection's
            // administrator disconnect; that is an intentional close.
            SessionEnd::Closed
        } else {
            failure.map_or(SessionEnd::Closed, SessionEnd::Lost)
        };
    }

    /// Log a failed write of `what`: one that timed out is a client that
    /// took none of it for the stall limit.
    fn write_failed(&mut self, what: &str, e: &std::io::Error, limits: &ConnectionLimits) {
        let problem = if matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ) {
            format!(
                "the client took none of {what} for {} s",
                limits.write_stall.as_secs()
            )
        } else {
            format!("writing {what}: {e}")
        };
        warn!("{}: closing: {problem}", self.peer);
        self.ending = SessionEnd::Lost(problem);
    }

    /// How long the connection may stay silent: `limits.idle` with a
    /// session, and without one (before Connect, after Disconnect) its own
    /// limit or `limits.idle` if that is shorter.
    fn idle_limit(&self, limits: &ConnectionLimits) -> Option<Duration> {
        match (&self.session, limits.idle) {
            (Some(_), idle) => idle,
            (None, Some(idle)) => Some(idle.min(limits.idle_without_session)),
            (None, None) => Some(limits.idle_without_session),
        }
    }

    /// The request id of the connection's next call on its client: odd,
    /// counting across the connection's sessions (spec 6.5).
    fn call_id(&mut self) -> u32 {
        let id = self.next_call;
        self.next_call = self.next_call.wrapping_add(2);
        id
    }

    /// Record and send one message of the server's own.
    fn send(&mut self, stream: &mut impl Write, message: &[u8]) -> std::io::Result<()> {
        self.record(false, message);
        stream.write_all(message)?;
        stream.flush()
    }

    /// Send the calls waiting for this connection's client, on its callback
    /// object: the results of Perform Script on Server with Callback, as
    /// `Perform` requests with op105 (script-runtime.md 11.4), and the
    /// administrator's messages as `Notify` calls (spec 6.5); and take the
    /// next step of a disconnect with notice that is due.
    fn send_calls(
        &mut self,
        stream: &mut impl Write,
        limits: &ConnectionLimits,
    ) -> std::io::Result<()> {
        let Some(session) = &self.session else {
            return Ok(());
        };
        let connection = session.connection_id;
        for transaction in self.shared.scripts.take_calls(connection) {
            let id = self.call_id();
            let call = om_protocol::build_perform(id, connection, b"", &transaction);
            debug!("{}: sending op105 as call {id}", self.peer);
            self.send(stream, &call)?;
        }
        self.farewell(stream, limits)?;
        self.file_closes(limits);
        self.send_notices(stream)
    }

    /// The next step of the administrator's close of each file the session
    /// has open (`ServerHandle::close_file`, spec 6.5): the request to
    /// close it (`0x0040`, naming it by the client's op63 id), the request
    /// again halfway through the grace time, the forced notice (`0x0870`)
    /// when it ends, and `limits.logout_wait` later, if the client still
    /// has it open, the immediate disconnect. A grace time of 0 sends the
    /// forced notice at once.
    fn file_closes(&mut self, limits: &ConnectionLimits) {
        let shared = &self.shared;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let now = Instant::now();
        let connection = session.connection_id;
        let mut notices = Vec::new();
        let mut leave = false;
        for (id, file) in &mut session.open {
            let Some(close) = shared.hosting.slot(*id).and_then(|slot| slot.closing()) else {
                continue;
            };
            let elapsed = now.saturating_duration_since(close.started);
            let ended = elapsed >= close.grace;
            let next = match file.close {
                CloseStep::None | CloseStep::Asked | CloseStep::AskedAgain if ended => {
                    CloseStep::Forced
                }
                CloseStep::None => CloseStep::Asked,
                CloseStep::Asked if elapsed >= close.grace / 2 => CloseStep::AskedAgain,
                CloseStep::Forced if elapsed >= close.grace + limits.logout_wait => {
                    leave = true;
                    continue;
                }
                _ => continue,
            };
            let name = &shared.identity.name;
            let message = if next == CloseStep::Forced {
                notify::AdminMessage::disconnect_all(*id, &close.text, name)
            } else {
                let registered = file
                    .hosted
                    .registrations()
                    .iter()
                    .find(|r| r.connection == connection)
                    .map(|r| r.client_file);
                match registered {
                    Some(client_file) => {
                        notify::AdminMessage::close_file(client_file, &close.text, name)
                    }
                    // A client that never registered the file is named
                    // the host's id for it (OpenMaker's choice).
                    None => notify::AdminMessage {
                        code: notify::attention::HOST_SERIAL | notify::attention::CLOSE_FILE,
                        file: *id as i32,
                        ..notify::AdminMessage::close_file(0, &close.text, name)
                    },
                }
            };
            file.close = next;
            notices.push(message);
        }
        for message in notices {
            // A client that takes no message is disconnected at the end of
            // its time to leave all the same.
            if let Ok(notice) = message.to_notification() {
                let _ = self.control.queue_notice(notice);
            }
        }
        if leave {
            info!(
                "{}: session 0x{connection:08x} kept a closing file open: disconnecting",
                self.peer
            );
            self.control.disconnect_now();
        }
    }

    /// Whether hosted file `file` is paused.
    fn file_paused(&self, file: u32) -> bool {
        self.shared
            .hosting
            .slot(file)
            .is_some_and(|slot| slot.status() == FileStatus::Paused)
    }

    /// The file a request must wait for: a `Perform` of the session on a
    /// paused file (an Open naming one included), or on a file a request
    /// is already held for, so that a file's requests keep their order.
    /// Everything else (`Tickle`, other files' requests) is answered at
    /// once, as FileMaker Server answers a Tickle while it holds a request
    /// (spec 6.1, 6.5).
    fn held_for(&self, msg: &[u8]) -> Option<u32> {
        let session = self.session.as_ref()?;
        let request = Request::parse(msg).ok()?;
        if request.object_key != OBJECT_KEY || request.operation != op::PERFORM {
            return None;
        }
        let perform = parse_perform_request(request.body)?;
        if perform.connection_id != session.connection_id {
            return None;
        }
        let header = Header::parse(perform.transaction)?;
        let file = if header.opcode == Opcode::OpenFile as u8 {
            let open = parse_open_request(perform.transaction).ok()?;
            self.shared.hosting.slot_named(&open.filename)?.id
        } else if session.open.contains_key(&header.file_id) {
            header.file_id
        } else {
            return None;
        };
        (self.file_paused(file) || self.held.iter().any(|held| held.file == file)).then_some(file)
    }

    /// Hold a request for paused file `file`, unanswered; past
    /// [`MAX_HELD`] or [`MAX_HELD_BYTES`] it is refused at once.
    fn hold(&mut self, file: u32, msg: &[u8]) -> Option<Vec<u8>> {
        if self.held.len() >= MAX_HELD || self.held_bytes + msg.len() > MAX_HELD_BYTES {
            warn!(
                "{}: refusing a request on paused file {file}: too many wait",
                self.peer
            );
            let request = Request::parse(msg).ok()?;
            return request
                .response_expected
                .then(|| exception(request.request_id, LIMIT_EXCEPTION));
        }
        debug!("{}: holding a request on paused file {file}", self.peer);
        self.held_bytes += msg.len();
        self.held.push_back(Held {
            file,
            msg: msg.to_vec(),
        });
        None
    }

    /// Answer the held requests whose files are no longer paused, in the
    /// order they came.
    fn answer_held(&mut self, stream: &mut impl Write) -> std::io::Result<()> {
        if self.held.is_empty() {
            return Ok(());
        }
        let mut waiting: std::collections::VecDeque<Held> = Default::default();
        for held in std::mem::take(&mut self.held) {
            if self.file_paused(held.file) || waiting.iter().any(|w| w.file == held.file) {
                waiting.push_back(held);
                continue;
            }
            self.held_bytes -= held.msg.len();
            debug!(
                "{}: answering a request held for file {}",
                self.peer, held.file
            );
            if let Some(reply) = self.handle_request(&held.msg) {
                self.send(stream, &reply)?;
            }
        }
        self.held = waiting;
        Ok(())
    }

    /// The next step of the session's disconnect with notice, once it is
    /// due (`ServerHandle::disconnect_client_with_notice`, spec 6.5): when
    /// the grace time ends, the forced notice to a client that still has a
    /// file open; `limits.logout_wait` later, a client that has not left is
    /// disconnected at once if it still has a file open, and otherwise
    /// called `Disconnect` on, its session ending, as FileMaker Server did.
    fn farewell(
        &mut self,
        stream: &mut impl Write,
        limits: &ConnectionLimits,
    ) -> std::io::Result<()> {
        let Some(session) = &self.session else {
            return Ok(());
        };
        let Some(farewell) = self.control.farewell() else {
            return Ok(());
        };
        let now = Instant::now();
        let connection = session.connection_id;
        let first_open = session.open.keys().min().copied();
        match farewell.leave_by {
            None if now >= farewell.grace_ends => {
                if let Some(file) = first_open {
                    info!(
                        "{}: session 0x{connection:08x} kept a file open past its grace time",
                        self.peer
                    );
                    let forced = notify::AdminMessage::disconnect_all(
                        file,
                        &farewell.text,
                        &self.shared.identity.name,
                    )
                    .to_notification();
                    // A client that takes no message is disconnected at the
                    // end of its time to leave all the same.
                    if let Ok(forced) = forced {
                        let _ = self.control.queue_notice(forced);
                    }
                }
                self.control.set_farewell(Some(Farewell {
                    leave_by: Some(now + limits.logout_wait),
                    ..farewell
                }));
            }
            Some(leave_by) if now >= leave_by => {
                if first_open.is_some() {
                    info!(
                        "{}: session 0x{connection:08x} did not leave when told: disconnecting",
                        self.peer
                    );
                    self.control.disconnect_now();
                } else {
                    let id = self.call_id();
                    info!(
                        "{}: ending session 0x{connection:08x}, its files closed, for the \
                         administrator",
                        self.peer
                    );
                    self.send(stream, &om_protocol::build_disconnect(id, connection, true))?;
                    self.end_session();
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Send the administrator's messages waiting for the session's client,
    /// one `Notify` call each, as FileMaker Server does: first locate the
    /// client's callback object once in the session, and send them once
    /// the client answers that it is there (spec 6.5).
    fn send_notices(&mut self, stream: &mut impl Write) -> std::io::Result<()> {
        let Some(session) = &self.session else {
            return Ok(());
        };
        if !self.control.has_notices() {
            return Ok(());
        }
        let connection = session.connection_id;
        match session.callback {
            Callback::Unlocated => {
                let id = self.call_id();
                if let Some(session) = &mut self.session {
                    session.callback = Callback::Locating(id);
                }
                debug!(
                    "{}: locating the client's callback object ({id})",
                    self.peer
                );
                self.send(
                    stream,
                    &om_giop::build_locate_request(id, notify::CALLBACK_KEY),
                )
            }
            Callback::Locating(_) => Ok(()),
            Callback::Here => {
                for notice in self.control.take_notices() {
                    let id = self.call_id();
                    let parms = write::notification_backchannel(std::slice::from_ref(&notice));
                    debug!(
                        "{}: sending notice 0x{:04x} as call {id}",
                        self.peer, notice.code
                    );
                    self.send(stream, &notify::build_notify(id, connection, &parms))?;
                }
                Ok(())
            }
            Callback::Absent => {
                let dropped = self.control.clear_notices();
                info!(
                    "{}: dropping {dropped} message(s) for a client without a callback object",
                    self.peer
                );
                Ok(())
            }
        }
    }

    /// The client's answer to the LocateRequest for its callback object.
    fn located(&mut self, msg: &[u8]) {
        let Ok((id, status)) = om_giop::parse_locate_reply(msg) else {
            debug!("{}: malformed LocateReply", self.peer);
            return;
        };
        let Some(session) = self
            .session
            .as_mut()
            .filter(|s| s.callback == Callback::Locating(id))
        else {
            debug!("{}: LocateReply {id} answers nothing waiting", self.peer);
            return;
        };
        session.callback = if status == om_giop::OBJECT_HERE {
            Callback::Here
        } else {
            info!(
                "{}: the client has no callback object (locate status {status})",
                self.peer
            );
            Callback::Absent
        };
    }

    /// Whether the change notifications waiting for this connection's
    /// session overflowed ([`Mailboxes::overflowed`]).
    fn notifications_overflowed(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| self.shared.mailboxes.overflowed(s.connection_id))
    }

    /// The client's answer to a call: an op105 `Perform` or a `Notify`.
    /// Nothing waits for it; it is logged.
    fn call_answered(&self, msg: &[u8]) {
        let Ok(reply) = om_giop::Reply::parse(msg) else {
            debug!("{}: malformed reply to a call", self.peer);
            return;
        };
        let callback = om_protocol::parse_perform_reply(msg)
            .ok()
            .and_then(|reply| script::parse_psos_result_reply(&reply.result).ok());
        debug!(
            "{}: the client answered call {}: {:?}{}",
            self.peer,
            reply.request_id,
            reply.status,
            callback.map_or(String::new(), |status| format!(", result {status:?}"))
        );
    }

    /// End the session, if any: release its record and catalog locks, its
    /// notification registrations, its waiting notifications and script
    /// callback results, its entry in the administrator's client list and
    /// its connection id; its transfers and staged records go with it.
    /// Also run while the thread unwinds from a panic (`Drop`), where a
    /// second panic aborts the server: so nothing here may panic, and
    /// every lock it takes is used even when poisoned.
    fn end_session(&mut self) {
        if let Some(session) = self.session.take() {
            if let Some(client) = &session.access {
                let events = &self.shared.events;
                for file in session.open.values() {
                    if let Some(account) = &file.account {
                        events.record(Event::client_closed(
                            client,
                            &file.hosted.name,
                            &account.name,
                        ));
                    }
                }
                events.record(Event::client_left(client, &self.ending));
            }
            let connection = session.connection_id;
            self.shared.scripts.forget(connection);
            for file in self.shared.hosting.files() {
                file.lock_table().release_connection(connection);
                file.registrations().retain(|r| r.connection != connection);
            }
            self.shared.mailboxes.close(connection);
            self.shared.clients.ended(connection);
            // After the entry is gone no message can be queued for the
            // session: what waits was for it alone, as was a disconnect
            // with notice under way.
            self.control.clear_notices();
            self.control.set_farewell(None);
            if !self.held.is_empty() {
                debug!(
                    "{}: dropping {} request(s) held for paused files",
                    self.peer,
                    self.held.len()
                );
                self.held.clear();
                self.held_bytes = 0;
            }
            self.shared.release_connection_id(connection);
            info!(
                "{}: session 0x{:08x} ended",
                self.peer, session.connection_id
            );
        }
    }

    /// The reply to one request, or `None` for a oneway request.
    fn handle_request(&mut self, msg: &[u8]) -> Option<Vec<u8>> {
        let request = match Request::parse(msg) {
            Ok(request) => request,
            Err(e) => {
                warn!("{}: malformed request: {e}", self.peer);
                return None;
            }
        };
        let id = request.request_id;
        let reply = if request.object_key != OBJECT_KEY {
            exception(id, "OBJECT_NOT_EXIST")
        } else {
            match request.operation {
                op::IS_A => {
                    // RCFMP12 itself, and OpenMaker's file list and
                    // authoring extensions.
                    let repo = parse_is_a_request(request.body);
                    let file_list = repo == Some(hosted_files::REPO_ID)
                        && !self.shared.no_file_list.load(Ordering::Relaxed);
                    let authoring = repo == Some(om_protocol::authoring::REPO_ID)
                        && !self.shared.no_authoring.load(Ordering::Relaxed);
                    build_is_a_reply(id, repo == Some(REPO_ID) || file_list || authoring)
                }
                op::CONNECT => self.connect(id, msg),
                op::PERFORM => self.perform(&request),
                hosted_files::LIST_FILES => self.list_files(&request),
                op::DISCONNECT => {
                    match (parse_disconnect_request(request.body), &self.session) {
                        (Some(conn), Some(s)) if conn == s.connection_id => self.end_session(),
                        _ => debug!("{}: Disconnect for an unknown connection", self.peer),
                    }
                    build_reply(id, ReplyStatus::NoException, &[], &[])
                }
                op::TICKLE => {
                    // The client's keepalive (network-protocol.md 6.1):
                    // answered with an empty inout sequence, which
                    // FileMaker Pro accepts.
                    let idle = parse_tickle_request(request.body)
                        .and_then(|tickle| parse_tickle_parms(tickle.parms));
                    debug!("{}: Tickle, sessions idle {idle:?}", self.peer);
                    build_tickle_reply(id, &[])
                }
                other => {
                    debug!(
                        "{}: unsupported operation {}",
                        self.peer,
                        String::from_utf8_lossy(other)
                    );
                    exception(id, "BAD_OPERATION")
                }
            }
        };
        request.response_expected.then_some(reply)
    }

    fn connect(&mut self, id: u32, msg: &[u8]) -> Vec<u8> {
        let request = match parse_connect_request(msg) {
            Ok(request) => request,
            Err(e) => {
                warn!("{}: Connect: {e}", self.peer);
                return exception(id, "MARSHAL");
            }
        };
        let machine = &request.machine;
        if machine.protocol_version <= MIN_PROTOCOL_EXCLUSIVE {
            warn!(
                "{}: refusing client protocol 0x{:02x}",
                self.peer, machine.protocol_version
            );
            return exception(id, "NO_PERMISSION");
        }
        self.end_session();

        let key: [u8; 16] = self.shared.random();
        let connection_id = self.shared.new_connection_id();
        info!(
            "{}: session 0x{connection_id:08x} for {:?} on {:?}",
            self.peer, machine.version_string, machine.machine_name
        );
        let client = crate::admission::client_key(self.address.ip());
        self.session = Some(Session {
            connection_id,
            key,
            name: machine.machine_name.clone(),
            open: HashMap::new(),
            transfers: Transfers::new(
                &self.shared.upload_memory,
                &self.shared.download_memory,
                client,
            ),
            temp: HashMap::new(),
            client,
            version: machine.version_string.clone(),
            address: self.address.ip(),
            access: None,
            callback: Callback::Unlocated,
            file_list: None,
            automatic_logins: Vec::new(),
        });
        self.shared
            .clients
            .connected(connection_id, self.address, machine, self.control.clone());

        let identity = &self.shared.identity;
        build_connect_reply(
            id,
            &ConnectReply {
                connection_id,
                connection_key: key,
                // The role bytes FileMaker Server 26 sends.
                exe_type: 0x22,
                app_type: 0x03,
                remote_name: identity.name.clone(),
                version_string: identity.version_string.clone(),
                system_language: 0x15,
                protocol_version: 0x20,
                install_code: identity.server_id.clone(),
                web_service_settings: identity.web_service_settings.clone(),
                standby_address: String::new(),
            },
        )
    }

    /// OpenMaker's file list (`om_protocol::hosted_files`): the names of
    /// the hosted files published ([`crate::Server::listing_file`]), in
    /// hosting order, and nothing else of them. Only for a connection this
    /// server handed out with `Connect`.
    fn list_files(&mut self, request: &Request<'_>) -> Vec<u8> {
        let id = request.request_id;
        let Some(list) = hosted_files::parse_list_files_request(request.body) else {
            return exception(id, "MARSHAL");
        };
        if !self
            .session
            .as_ref()
            .is_some_and(|s| s.connection_id == list.connection_id)
        {
            debug!("{}: file list for an unknown connection", self.peer);
            return exception(id, "BAD_PARAM");
        }
        if list.version < hosted_files::VERSION {
            return exception(id, "NO_IMPLEMENT");
        }
        let shared = &self.shared;
        if let Some(body) = shared
            .file_list_override
            .read()
            .expect("file list override lock")
            .clone()
        {
            return build_reply(id, ReplyStatus::NoException, &[], &body);
        }
        let listed = shared.listed.read().expect("listed files lock");
        // Files that are closing, closed or opening cannot be opened.
        let names: Vec<&str> = shared
            .hosting
            .slots()
            .iter()
            .filter(|slot| matches!(slot.status(), FileStatus::Open | FileStatus::Paused))
            .map(|slot| slot.name.as_str())
            .filter(|name| {
                listed.contains(&name.to_lowercase()) && hosted_files::is_listable_name(name)
            })
            .take(hosted_files::MAX_FILES)
            .collect();
        info!(
            "{}: listed {} of {} hosted files",
            self.peer,
            names.len(),
            shared.hosting.slots().len()
        );
        hosted_files::build_list_files_reply(id, &names)
    }

    fn perform(&mut self, request: &Request<'_>) -> Vec<u8> {
        let id = request.request_id;
        let Some(perform) = parse_perform_request(request.body) else {
            return exception(id, "MARSHAL");
        };
        if !MAX_RETURN_RANGE.contains(&perform.max_return) {
            return exception(id, "BAD_PARAM");
        }
        let Some(header) = Header::parse(perform.transaction) else {
            return exception(id, "BAD_PARAM");
        };
        let Some(session) = self
            .session
            .as_mut()
            .filter(|s| s.connection_id == perform.connection_id)
        else {
            debug!("{}: Perform for an unknown connection", self.peer);
            return exception(id, "BAD_PARAM");
        };
        let txn = perform.transaction;
        let peer = &self.peer;
        let shared = &self.shared;
        debug!(
            "{peer}: op{} {} file {} ({} bytes, maxreturn {})",
            header.opcode,
            Opcode::name(header.opcode).unwrap_or("(unknown)"),
            header.file_id,
            txn.len(),
            perform.max_return
        );
        #[cfg(debug_assertions)]
        if let Some(test) = shared
            .test_panic
            .get()
            .filter(|t| t.opcode == header.opcode && t.due())
        {
            test.fire(shared, session);
        }
        let result = match header.opcode {
            code if code == Opcode::OpenFile as u8 => open(shared, peer, session, txn),
            code if code == Opcode::GetFileList as u8
                && !shared.no_get_file_list.load(Ordering::Relaxed) =>
            {
                file_list(shared, peer, session, &header, txn, perform.max_return)
            }
            code if code == Opcode::Login as u8 => {
                login(shared, peer, self.address.ip(), session, &header, txn)
            }
            code if code == Opcode::DownloadWithLock as u8 => {
                download(shared, peer, session, &header, txn, perform.max_return)
            }
            code if code == Opcode::DownloadList as u8 => {
                download_list(shared, peer, session, &header, txn, perform.max_return)
            }
            code if code == Opcode::QueryProgram as u8 => {
                query(shared, peer, session, &header, txn, perform.max_return)
            }
            code if code == Opcode::RequestNotification as u8 => {
                request_notification(peer, session, &header, txn)
            }
            code if code == Opcode::HostCreateRecord as u8 => {
                if create_refused_for_test(shared) {
                    Ok(write::CreateRecordReply {
                        conflict: None,
                        status: u32::from(ACCESS_DENIED),
                        record: vec![vec![0x00]],
                    }
                    .to_result())
                } else {
                    create_record(shared, peer, session, &header, txn)
                }
            }
            code if code == Opcode::HostGetSerial as u8 => {
                get_serial(shared, session, &header, txn)
            }
            code if code == Opcode::UploadListToHostTempLocation as u8 => {
                upload(shared, peer, session, &header, txn, perform.max_return)
            }
            code if code == Opcode::UploadBinaryData as u8 => {
                upload_binary(shared, peer, session, &header, txn)
            }
            code if code == Opcode::AdjustReferenceCount as u8 => {
                adjust_reference_count(peer, session, &header, txn)
            }
            code if code == Opcode::DownloadThumbnail as u8 => {
                thumbnail(session, &header, txn, perform.max_return)
            }
            code if code == Opcode::HostCommit as u8 => {
                commit(shared, peer, session, &header, txn, perform.max_return)
            }
            code if code == Opcode::PerformScriptOnServer as u8 => {
                perform_script(shared, peer, session, &header, txn)
            }
            // FileMaker Server answers 21 (not supported) to every op103 and
            // FileMaker Pro ignores it (write-path.md 3.5).
            code if code == Opcode::TransactInfo as u8 => write::parse_transact_info_request(txn)
                .map(|_| {
                    ResultBuilder::new()
                        .push_u32(write::TRANSACT_INFO_REPLY)
                        .finish()
                }),
            code if code == Opcode::Lock as u8 => lock(shared, session, &header, txn),
            // FileMaker Server answers Unlock and KillAllLocks with the
            // request's own header (idletime zeroed), so the status a client
            // pops is 0 (spec 5.6.10, write-path.md 4.3-4.4).
            code if code == Opcode::Unlock as u8 => {
                write::parse_unlock_request(txn).map(|request| {
                    if request.closes_file() {
                        close_file(shared, peer, session, header.file_id);
                    } else if let Some(file) = session
                        .open
                        .get(&header.file_id)
                        .filter(|f| f.account.is_some())
                    {
                        let release = ReleaseLock {
                            connection: session.connection_id,
                            request,
                        };
                        let outcome = shared
                            .engine
                            .submit(&file.hosted, Operation::ReleaseLock(release));
                        outcome.notices.deliver(&file.hosted, &shared.mailboxes);
                    }
                    write::echo_header(&header)
                })
            }
            code if code == Opcode::Upgrade as u8 => upgrade(shared, session, &header, txn),
            // The host's clock, in its local time (schema-path.md 3.1).
            code if code == Opcode::HostTimeStamp as u8 => {
                #[cfg(debug_assertions)]
                let failing = shared.failing_timestamps.load(Ordering::Relaxed);
                #[cfg(not(debug_assertions))]
                let failing = false;
                if failing {
                    Err(PerformError::Status("test failure of op82".into()))
                } else {
                    Ok(schema::host_timestamp_reply(&host_time_key()))
                }
            }
            code if code == Opcode::DownloadFromHost as u8 => {
                download_from_host(shared, peer, session, &header, txn, perform.max_return)
            }
            code if code == Opcode::UploadToHost as u8 => {
                upload(shared, peer, session, &header, txn, perform.max_return)
            }
            code if code == Opcode::HostUpdateTable as u8 => {
                update_table(shared, peer, session, &header, txn)
            }
            code if code == Opcode::OpenMakerAuthoring as u8
                && !shared.no_authoring.load(Ordering::Relaxed) =>
            {
                let file = session.open.get(&header.file_id);
                let caller = authoring::Caller {
                    file: file.map(|f| &f.hosted),
                    account: file.and_then(|f| f.account.as_deref()),
                    connection: session.connection_id,
                    session_name: &session.name,
                };
                // A new definition is read, for a caller that may send it,
                // within what the client may still hold, as the other
                // uploads' trees are.
                authoring::perform(shared, peer, &caller, txn, || {
                    reserve_tree(shared, session, txn.len())
                })
            }
            code if code == Opcode::KillAllLocks as u8 => write::parse_kill_all_locks_request(txn)
                .map(|request| {
                    if let Some(file) = session.open.get(&header.file_id) {
                        let release = ReleaseLocks {
                            connection: session.connection_id,
                            request,
                        };
                        let operation = Operation::ReleaseLocks(release);
                        shared.engine.submit(&file.hosted, operation);
                    }
                    write::echo_header(&header)
                }),
            code if code == Opcode::Logout as u8 => {
                clear_file_session(shared, session, header.file_id);
                Ok(ResultBuilder::new().push_u16(0).finish())
            }
            other => {
                info!(
                    "{peer}: unsupported op{other} {}",
                    Opcode::name(other).unwrap_or("(unknown)")
                );
                Ok(ResultBuilder::new().push_u16(UNSUPPORTED).finish())
            }
        };
        // Changes other clients made go out in the backchannel (write-path.md
        // 6.4), as many as the reply leaves room for in `maxreturn`; an
        // exception carries none, so they wait for the next reply.
        let notifications = match &result {
            Ok(result) => {
                let room = (perform.max_return as usize).saturating_sub(result.len());
                shared.mailboxes.take(session.connection_id, room)
            }
            Err(_) => Vec::new(),
        };
        let backchannel = if notifications.is_empty() {
            BACKCHANNEL.to_vec()
        } else {
            write::notification_backchannel(&notifications)
        };
        match result {
            Ok(result) => build_perform_reply(id, &result, &backchannel),
            Err(PerformError::Status(status)) if status == LIMIT_EXCEPTION => {
                warn!("{peer}: op{}: refused at a connection limit", header.opcode);
                exception(id, LIMIT_EXCEPTION)
            }
            Err(e) => {
                warn!("{peer}: op{}: {e}", header.opcode);
                exception(id, "MARSHAL")
            }
        }
    }
}

/// However the connection's thread leaves [`Connection::serve`], by a
/// panic in a request handler too, its session ends: a panic costs that
/// connection and nothing else. (Its recording's files close with it.)
impl Drop for Connection {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.ending = SessionEnd::Panic;
        }
        self.end_session();
    }
}

/// A request that panics, for tests of what a panicking connection leaves
/// behind ([`crate::Server::panicking_on`]). Only in debug builds.
#[cfg(debug_assertions)]
#[derive(Debug)]
pub(crate) struct TestPanic {
    /// The opcode whose Perform panics.
    pub opcode: u8,
    /// Panic while holding every shared lock a session's end takes.
    pub holding_locks: bool,
    /// How many Performs of `opcode` are still answered before one panics.
    pub skip: std::sync::atomic::AtomicU32,
    /// Only that one panics; the later ones are answered.
    pub once: bool,
}

#[cfg(debug_assertions)]
impl TestPanic {
    /// Whether this Perform of the opcode is the one that panics: those
    /// `skip` counts are answered first.
    fn due(&self) -> bool {
        let due = self
            .skip
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_err();
        if due && self.once {
            self.skip.store(u32::MAX, Ordering::Relaxed);
        }
        due
    }

    fn fire(&self, shared: &Shared, session: &Session) -> ! {
        if self.holding_locks {
            // In the order the write path takes them (`crate::writes`).
            let files: Vec<&Hosted> = session.open.values().map(|f| &*f.hosted).collect();
            let _tables: Vec<_> = files.iter().map(|file| file.lock_table()).collect();
            let _registrations: Vec<_> = files.iter().map(|file| file.registrations()).collect();
            let _mailboxes = shared.mailboxes.boxes();
            let _calls = shared.scripts.queued_calls();
            let _ids = shared.connection_ids();
            panic!("test panic in op{} holding the shared locks", self.opcode);
        }
        panic!("test panic in op{}", self.opcode);
    }
}

/// Holds Perform Script on Server requests between their admission to
/// their file and their script's start, until it is opened: a test fixture
/// for a close that begins in between ([`crate::Server::holding_scripts`]).
/// Only in debug builds.
#[cfg(debug_assertions)]
#[derive(Clone, Debug, Default)]
pub struct ScriptGate(Arc<(Mutex<GateState>, std::sync::Condvar)>);

#[cfg(debug_assertions)]
#[derive(Debug, Default)]
struct GateState {
    admitted: usize,
    open: bool,
}

#[cfg(debug_assertions)]
impl ScriptGate {
    /// Wait up to `timeout` until a request is held; whether one is.
    pub fn wait_admitted(&self, timeout: Duration) -> bool {
        let (state, changed) = &*self.0;
        let state = state.lock().unwrap_or_else(PoisonError::into_inner);
        let (state, _) = changed
            .wait_timeout_while(state, timeout, |state| state.admitted == 0)
            .unwrap_or_else(PoisonError::into_inner);
        state.admitted > 0
    }

    /// Let the held requests and every later one go on.
    pub fn open(&self) {
        let (state, changed) = &*self.0;
        state.lock().unwrap_or_else(PoisonError::into_inner).open = true;
        changed.notify_all();
    }

    /// Hold an admitted request until the gate is open.
    fn hold(&self) {
        let (state, changed) = &*self.0;
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        state.admitted += 1;
        changed.notify_all();
        while !state.open {
            state = changed.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// op78 Open: hand out a fresh per-file nonce (spec 5.6.4).
/// GetFileList (op77), as FileMaker Server answers FileMaker Pro's Hosts
/// window (network-protocol.md 5.3): the files this server publishes
/// ([`crate::Server::listing_file`], the same files its own listing
/// extension lists) that can be opened now, as a list tree whose entries,
/// keyed `01`, `02`, ... in hosting order, hold the file's id `01` (the id
/// Open returns) and its name `10`. No sign-in is asked for: OpenMaker
/// Server has no "Filter Databases", so every client gets the same list.
/// Block New Users refuses a new user with 1638, as Open does.
///
/// The list is paged like a download (5.6.6c) so that each reply fits the
/// client's `maxreturn`; every page carries the list's count. A reply that
/// cannot fit even its fixed fields and one whole chunk of the list is
/// refused with a system exception, never sent larger than asked for.
fn file_list(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    header: &Header,
    txn: &[u8],
    max_return: u32,
) -> Result<Vec<u8>, PerformError> {
    let request = parse_file_list_request(txn)?;
    let host = &shared.identity.name;
    let reply = |slots, resume, count, error| FileListReply {
        slots,
        resume,
        count,
        host_name: host.clone(),
        error,
    };
    if session.open.is_empty() && shared.block_new_users.load(Ordering::Acquire) {
        info!("{peer}: GetFileList refused, new users are blocked");
        let refused = reply(Vec::new(), Resume::complete(), 0, OPEN_BLOCKED);
        return file_list_within(peer, &refused, max_return);
    }
    // What `maxreturn` leaves for the list after the reply's other fields
    // (their size does not depend on the page).
    let fixed = reply(Vec::new(), Resume::complete(), 0, 0)
        .to_result()
        .len();
    let limit = page_limit(max_return).saturating_sub(fixed);
    let owner = Owner::of(header);
    if request.resume.has_more() {
        let page = match session.transfers.next(owner, &request.resume, limit)? {
            Some((page, resume)) => {
                let count = session
                    .file_list
                    .filter(|(perform, _)| *perform == request.resume.perform)
                    .map_or(0, |(_, count)| count);
                reply(page, resume, count, 0)
            }
            None => reply(Vec::new(), unknown_cursor(peer, &request.resume), 0, 0),
        };
        return file_list_within(peer, &page, max_return);
    }
    let listed = shared.listed.read().expect("listed files lock");
    // Files that are closing, closed or opening cannot be opened.
    let entries: Vec<om_hbam::OwnedNode> = shared
        .hosting
        .slots()
        .iter()
        .filter(|slot| matches!(slot.status(), FileStatus::Open | FileStatus::Paused))
        .filter(|slot| {
            listed.contains(&slot.name.to_lowercase()) && hosted_files::is_listable_name(&slot.name)
        })
        .take(hosted_files::MAX_FILES)
        .enumerate()
        .map(|(index, slot)| {
            om_hbam::OwnedNode::list(
                om_hbam::serial::encode(index as u64 + 1),
                vec![
                    om_hbam::OwnedNode::leaf([FILE_LIST_ID], slot.id.to_be_bytes()),
                    om_scripts::text_node(&[FILE_LIST_NAME], &slot.name),
                ],
            )
        })
        .collect();
    drop(listed);
    let count = u16::try_from(entries.len()).unwrap_or(u16::MAX);
    let slots = om_hbam::OwnedNode::list(Vec::new(), entries)
        .to_wire()
        .ok_or(PerformError::Truncated)?;
    info!(
        "{peer}: GetFileList ({:?}): listed {count} of {} hosted files",
        request.share_type,
        shared.hosting.slots().len()
    );
    let (page, resume) = session
        .transfers
        .start(owner, slots, limit, || shared.handle());
    if resume.has_more() {
        session.file_list = Some((resume.perform, count));
    }
    file_list_within(peer, &reply(page, resume, count, 0), max_return)
}

/// An op77 page's result stack, or a refusal when it is larger than the
/// client's `maxreturn`: a page holds at least one whole chunk of the list.
fn file_list_within(
    peer: &str,
    reply: &FileListReply,
    max_return: u32,
) -> Result<Vec<u8>, PerformError> {
    let result = reply.to_result();
    if result.len() > max_return as usize {
        warn!(
            "{peer}: GetFileList: a {}-byte page does not fit maxreturn {max_return}",
            result.len()
        );
        return Err(Refused.into());
    }
    Ok(result)
}

/// An op77 list entry's children: the file's id and its name
/// (network-protocol.md 5.3).
const FILE_LIST_ID: u8 = 0x01;
const FILE_LIST_NAME: u8 = 0x10;

fn open(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    let request = parse_open_request(txn)?;
    let delay = shared
        .open_delays
        .read()
        .expect("open delays lock")
        .get(&request.filename.to_lowercase())
        .copied();
    if let Some(delay) = delay {
        std::thread::sleep(delay);
    }
    // Block New Users refuses a new user whatever file it names (whether
    // FileMaker Server looks for the file first is unmeasured); a session
    // with a file open is not a new user (OpenMaker's choice,
    // `ServerHandle::set_block_new_users`).
    if session.open.is_empty() && shared.block_new_users.load(Ordering::Acquire) {
        info!(
            "{peer}: Open {:?}: refused, new users are blocked",
            request.filename
        );
        return Ok(OpenReply::refused(OPEN_BLOCKED, &shared.identity.name).to_result());
    }
    let Some(slot) = shared.hosting.slot_named(&request.filename) else {
        info!("{peer}: Open {:?}: not hosted", request.filename);
        return Ok(OpenReply::error_result(OPEN_FILE_MISSING, None));
    };
    // The client is registered as having the file open under the file's
    // slot lock, so that a close finishing meanwhile cannot leave it a file
    // no longer served. A file that is closing, closed or opening is
    // refused as one the server does not host (OpenMaker's choice; what
    // FileMaker Server answers is unmeasured).
    let nonce = u32::from_be_bytes(shared.random());
    let admitted = slot.admit(|hosted| {
        if session.open.contains_key(&hosted.id) {
            clear_file_session(shared, session, hosted.id);
        }
        session.open.insert(
            hosted.id,
            OpenFile {
                hosted: hosted.clone(),
                nonce,
                account: None,
                close: CloseStep::None,
                withheld: Default::default(),
            },
        );
        shared
            .clients
            .opened(session.connection_id, hosted.id, &hosted.name);
        hosted.clone()
    });
    let hosted = match admitted {
        Ok(hosted) => hosted,
        Err(status) => {
            info!(
                "{peer}: Open {:?}: refused, the file is {}",
                request.filename,
                status.filemaker_name()
            );
            return Ok(OpenReply::error_result(OPEN_FILE_MISSING, None));
        }
    };
    info!("{peer}: opened {:?} as file {}", hosted.name, hosted.id);
    Ok(OpenReply {
        status: 0,
        server_message: shared.identity.name.clone(),
        file_nonce: nonce,
        flag: false,
        file_id: hosted.id,
        // The values FileMaker Server 26 sends; their meaning is not established.
        file_aa4: 9,
        marker: 0,
        file_text: String::new(),
        flags: 0x00a1_0000,
        session_value: u64::from_be_bytes(shared.random()),
    }
    .to_result())
}

/// The client closed file `file_id` (an op62 Unlock of its root, spec
/// 6.5): what the session holds in it is released, as at Logout, and the
/// file is no longer open in the session. The session goes on.
fn close_file(shared: &Shared, peer: &str, session: &mut Session, file_id: u32) {
    clear_file_session(shared, session, file_id);
    if let Some(file) = session.open.remove(&file_id) {
        shared.clients.closed(session.connection_id, file_id);
        info!("{peer}: closed {:?}", file.hosted.name);
    }
}

/// End authority for this file before logout or changing identities.
fn clear_file_session(shared: &Shared, session: &mut Session, file_id: u32) {
    if let Some(file) = session.open.get_mut(&file_id) {
        // What the old account was served withheld is its own.
        file.withheld.lock().expect("withheld records lock").clear();
        if let (Some(account), Some(client)) = (file.account.take(), &session.access) {
            shared.events.record(Event::client_closed(
                client,
                &file.hosted.name,
                &account.name,
            ));
        }
        file.hosted
            .lock_table()
            .release_connection(session.connection_id);
        let mut registrations = file.hosted.registrations();
        registrations.retain(|r| r.connection != session.connection_id);
        shared
            .mailboxes
            .clear_file(session.connection_id, file.hosted.id);
        session.temp.remove(&file_id);
        // Only this file's transfers go: continuing one is refused, and the
        // session's transfers on its other files go on.
        session.transfers.drop_file(file_id);
        shared.clients.logged_out(session.connection_id, file_id);
    }
}

/// op80 Login from `client`: recover the credentials and check them against
/// the file's own account (spec 5.4; `hosting::Eligibility`). A server
/// limits how often logins fail by address and by account name
/// (`login_limits`): a login past a limit is answered 214 without its
/// password being checked. A local client's private host has no limits.
fn login(
    shared: &Shared,
    peer: &str,
    client: IpAddr,
    session: &mut Session,
    header: &Header,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    if session
        .open
        .get(&header.file_id)
        .is_some_and(|file| file.account.is_some())
    {
        clear_file_session(shared, session, header.file_id);
    }
    let Some(file) = session.open.get_mut(&header.file_id) else {
        return Ok(rejected_login(LOGIN_REJECTED));
    };
    // A wrong key yields undecodable text: treat it as a failed login.
    let request = parse_login_request(txn, file.nonce, file.hosted.internal_creation, &session.key);
    let named = request.as_ref().ok().map(|login| login.account.clone());
    let empty_password = request
        .as_ref()
        .is_ok_and(|login| login.password.is_empty());
    let session_name = request
        .as_ref()
        .ok()
        .map(|login| login.environment.session_name.clone());
    // Counted before the password is checked, and refused unchecked when
    // its address or account has failed too often.
    let attempt = if shared.local {
        None
    } else {
        let limits = &shared.login_limits;
        let begun = if empty_password {
            limits.begin_empty(client, &file.hosted.name, named.as_deref())
        } else {
            limits.begin(client, &file.hosted.name, named.as_deref())
        };
        match begun {
            Ok(attempt) => Some(attempt),
            Err(refusal) => {
                debug!("{peer}: login to {:?} refused: {refusal}", file.hosted.name);
                return Ok(rejected_login(LOGIN_TOO_MANY_ATTEMPTS));
            }
        }
    };
    let accepted = match request {
        Ok(login) if login.login_type == 0 && !login.has_new_password => {
            // The password is checked against the one the file stores for
            // the account, whose record the reply names (FileMaker Pro takes
            // the session's privileges from it). The file admits only
            // accounts it may (an active FileMaker-file account, no password
            // change due, in a set the host enforces), with a correct
            // password, before any session exists.
            let account = file.hosted.verify_password(&login.account, &login.password);
            let checked = account.ok_or(LOGIN_REJECTED).and_then(|account| {
                let today = jiff::Zoned::now().date();
                let eligibility = if shared.local {
                    account.eligibility(today)
                } else {
                    account.server_eligibility(today, shared.custom_sets.load(Ordering::Relaxed))
                };
                let code = match eligibility {
                    Eligibility::Eligible => return Ok(account),
                    Eligibility::Guest | Eligibility::External => LOGIN_REJECTED,
                    Eligibility::Inactive => LOGIN_INACTIVE_ACCOUNT,
                    Eligibility::PasswordChangeDue => LOGIN_PASSWORD_CHANGE_DUE,
                    Eligibility::UnreadableSettings => LOGIN_UNREADABLE_ACCOUNT,
                    Eligibility::UnsupportedPrivileges => LOGIN_UNSUPPORTED_PRIVILEGES,
                };
                Err(code)
            });
            info!(
                "{peer}: login to {:?} as {:?}: {}",
                file.hosted.name,
                login.account,
                match &checked {
                    Ok(_) => "accepted",
                    Err(LOGIN_REJECTED) => "rejected",
                    Err(_) => "refused",
                }
            );
            checked.map(|account| (account, login.environment.session_name))
        }
        Ok(login) => {
            info!(
                "{peer}: login type {} (new password {}) is not supported",
                login.login_type, login.has_new_password
            );
            Err(LOGIN_REJECTED)
        }
        Err(e) => {
            info!("{peer}: undecodable login: {e}");
            Err(LOGIN_REJECTED)
        }
    };
    let used_up = match (attempt, &accepted) {
        (Some(attempt), Err(LOGIN_REJECTED)) => {
            if empty_password {
                if session.automatic_logins.len() >= MAX_AUTOMATIC_LOGINS {
                    // The oldest stays counted.
                    let _ = session.automatic_logins.remove(0);
                }
                session.automatic_logins.push(attempt);
            }
            shared.login_limits.failed(attempt, peer, named.as_deref())
        }
        (Some(attempt), outcome) => {
            shared.login_limits.passed(attempt, outcome.is_ok());
            if outcome.is_ok() {
                shared
                    .login_limits
                    .take_back(session.automatic_logins.drain(..));
            }
            Default::default()
        }
        (None, _) => Default::default(),
    };
    if accepted.is_err() {
        // Named by the session name this Login gave, or else its machine.
        let name = session_name.as_deref().unwrap_or(&session.name);
        let client = ClientLabel::new(name, &session.name, session.address);
        let (events, file, account) = (&shared.events, &file.hosted.name, named.as_deref());
        events.record(Event::login_failed(&client, file, account.unwrap_or("")));
        if used_up.address {
            events.record(Event::address_locked(&client, file));
        }
        if used_up.account {
            events.record(Event::account_locked(&client, file, account.unwrap_or("")));
        }
    }
    Ok(match accepted {
        Ok((account, session_name)) => {
            // The result key is the account's record key framed as a
            // one-segment key path: `01 02` for test project's account 2,
            // as FileMaker Server sends (spec 5.6.5).
            let mut result_key = vec![u8::try_from(account.key.len()).unwrap_or(0)];
            result_key.extend_from_slice(&account.key);
            shared.clients.logged_in(
                session.connection_id,
                header.file_id,
                &account.name,
                &session_name,
            );
            let client = session.access.get_or_insert_with(|| {
                shared.events.record(Event::client_connected(
                    &session_name,
                    &session.name,
                    session.address,
                    &session.version,
                ));
                let label = ClientLabel::new(&session_name, &session.name, session.address);
                shared.clients.labelled(session.connection_id, &label);
                label
            });
            shared.events.record(Event::client_opened(
                client,
                &file.hosted.name,
                &account.name,
            ));
            file.withheld.lock().expect("withheld records lock").clear();
            file.account = Some(account);
            if shared.local {
                // Deferred work the local file's opening finished is saved
                // now that an account it admits has logged in.
                file.hosted.commit_recovery();
            }
            LoginReply {
                error_code: 0,
                result_key,
                version: Some(PROTOCOL_VERSION),
            }
            .to_result()
        }
        Err(code) => rejected_login(code),
    })
}

/// A refused Login's reply: the version and an empty key under the error,
/// 5 bytes, as FileMaker Server sends it (network-protocol.md 5.6.5).
fn rejected_login(error_code: u16) -> Vec<u8> {
    LoginReply {
        error_code,
        result_key: Vec::new(),
        version: Some(PROTOCOL_VERSION),
    }
    .to_result()
}

/// The open file a download names, if this session may read `path` in it.
/// Before Login only the status area and account catalog header are readable.
/// After Login, restricted sessions cannot read account-member parents or
/// whole members except their own, or read a member's `04` and `06` children.
/// Configured download refusals apply to every session.
fn readable<'s>(
    shared: &Shared,
    peer: &str,
    session: &'s Session,
    header: &Header,
    path: &[Vec<u8>],
) -> Option<&'s OpenFile> {
    let file = session.open.get(&header.file_id)?;
    let pre_login = PRE_LOGIN_PATHS.iter().any(|allowed| {
        path.len() >= allowed.len() && allowed.iter().zip(path).all(|(a, p)| *a == p.as_slice())
    });
    if file.account.is_none() && !pre_login {
        info!("{peer}: refusing a download of {path:02x?} before Login");
        return None;
    }
    if !full_access(file)
        && credential_path(path.iter().map(Vec::as_slice))
        && !own_account_member(file, path)
    {
        return None;
    }
    let refused = shared
        .refused_downloads
        .read()
        .expect("refused downloads lock")
        .iter()
        .any(|refused| path.starts_with(refused));
    if refused {
        info!("{peer}: refusing a download of {path:02x?}");
        return None;
    }
    Some(file)
}

/// True if a subtree can contain an account password verifier. The account
/// record key is variable, so its position is a wildcard.
fn credential_path<'a>(path: impl IntoIterator<Item = &'a [u8]>) -> bool {
    let path: Vec<&[u8]> = path.into_iter().collect();
    let fixed: [&[u8]; 3] = [&[23], &[1], &[5]];
    for (actual, expected) in path.iter().zip(fixed) {
        if *actual != expected {
            return false;
        }
    }
    path.len() <= 4 || matches!(path[4], [4] | [6])
}

/// FileMaker Pro fetches its own account member after Login to finish opening
/// a hosted file. The whole-member exception is exact; the download handlers
/// remove its password, checksum and unknown children.
fn own_account_member(file: &OpenFile, path: &[Vec<u8>]) -> bool {
    let Some(account) = file.account.as_deref() else {
        return false;
    };
    account_member_path(path.iter().map(Vec::as_slice), &account.key)
}

fn account_member_path<'a>(path: impl IntoIterator<Item = &'a [u8]>, key: &[u8]) -> bool {
    let mut path = path.into_iter();
    matches!(
        (path.next(), path.next(), path.next(), path.next(), path.next()),
        (Some([23]), Some([1]), Some([5]), Some(member), None) if member == key
    )
}

/// Known metadata retained in a restricted whole-member reply. Direct child
/// reads keep the existing rule in `credential_path`.
fn account_metadata(segment: &[u8]) -> bool {
    matches!(
        segment,
        [2] | [3]
            | [0x0a]
            | [0x0b]
            | [0x10]
            | [0xd8]
            | [0xd9]
            | [0xfc]
            | [0xfc, 1]
            | [0xfc, 2]
            | [0xfc, 3]
    )
}

/// Keep account member credentials out of downloads to a restricted account.
fn filter_security_metadata(file: &OpenFile, path: &[Vec<u8>], node: &mut OwnedNode) {
    if own_account_member(file, path) && !full_access(file) {
        node.children
            .retain(|child| account_metadata(&child.segment));
    }
}

/// A copy of the node at `path` in `file`, or `None` when it has no such key.
fn read_node(file: &OpenFile, path: &[Vec<u8>]) -> Option<OwnedNode> {
    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let store = file.hosted.read_store();
    let mut node = store.node(&segments).cloned()?;
    filter_security_metadata(file, path, &mut node);
    match view_filter(file, &store) {
        Some(view) => {
            let node = view.filter(&store, &segments, node);
            note_withheld(file, &view);
            node
        }
        None => Some(node),
    }
}

/// Note in `file`'s session what `view` withheld (`views::Withheld`).
fn note_withheld(file: &OpenFile, view: &crate::views::ViewFilter) {
    file.withheld
        .lock()
        .expect("withheld records lock")
        .note(view);
}

/// The view limits of `file`'s account (`views::ViewFilter`), when its
/// privilege set limits viewing any table's records by a calculation.
fn view_filter(file: &OpenFile, store: &crate::store::Store) -> Option<crate::views::ViewFilter> {
    let account = file.account.as_deref()?;
    crate::views::ViewFilter::new(store, account.access.as_ref()?, &account.name)
}

/// Snapshot only the requested children. The key list's keys and the
/// parent's children are both in key order, so the two are walked side by
/// side, the keys read from the list's bytes rather than collected (a list
/// of short keys collected takes many times its size). Preserve the
/// parent's order and release the store lock before encoding or paging the
/// reply.
fn read_selected_node(
    shared: &Shared,
    file: &OpenFile,
    path: &[Vec<u8>],
    keys: &KeyList<'_>,
) -> Option<OwnedNode> {
    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let store = file.hosted.read_store();
    let parent = store.node(&segments)?;
    let mut keys = keys.keys().peekable();
    let mut selected = OwnedNode {
        segment: parent.segment.clone(),
        value: parent.value.clone(),
        push: parent.push,
        children: parent
            .children
            .iter()
            .filter(|child| {
                let child = child.segment.as_slice();
                while keys.next_if(|key| *key < child).is_some() {}
                keys.next_if(|key| *key == child).is_some()
            })
            .cloned()
            .collect(),
    };
    for omitted in shared
        .omitted_list_paths
        .read()
        .expect("omitted list paths lock")
        .iter()
    {
        if let Some(relative) = omitted.strip_prefix(path) {
            if relative.is_empty() {
                selected.children.clear();
            } else {
                let refs: Vec<&[u8]> = relative.iter().map(Vec::as_slice).collect();
                selected.remove(&refs);
            }
        }
    }
    filter_security_metadata(file, path, &mut selected);
    match view_filter(file, &store) {
        Some(view) => {
            let node = view.filter(&store, &segments, selected);
            note_withheld(file, &view);
            node
        }
        None => Some(selected),
    }
}

impl Shared {
    /// A fresh nonzero transfer handle.
    fn handle(&self) -> u64 {
        loop {
            let handle = u64::from_be_bytes(self.random());
            if handle != 0 {
                return handle;
            }
        }
    }
}

/// A refused transfer fails the request with [`LIMIT_EXCEPTION`]: `perform`
/// answers this status with that exception.
impl From<Refused> for PerformError {
    fn from(_: Refused) -> Self {
        PerformError::Status(LIMIT_EXCEPTION.to_owned())
    }
}

/// The reply to a continuation whose cursor this connection never issued (or
/// already finished): an empty last page (spec 5.6.6c step 1).
fn unknown_cursor(peer: &str, resume: &Resume) -> Resume {
    debug!(
        "{peer}: continuation for unknown cursor {:#x}",
        resume.cursor
    );
    Resume::complete()
}

/// op90 Download With Lock: the target's subtree in wire form, paged when it
/// does not fit one reply, or its value inline when the key holds a value
/// (spec 5.6.6, 5.6.6c).
fn download(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    header: &Header,
    txn: &[u8],
    max_return: u32,
) -> Result<Vec<u8>, PerformError> {
    let request = parse_download_request(txn)?;
    let reply = download_reply(shared, peer, session, header, &request, max_return)?;
    Ok(reply.to_result_within(max_return as usize))
}

/// op15 Download From Host: op90 without the lock; FileMaker Pro reads the
/// catalogs it holds locked this way (schema-path.md 3.2). The reply lacks
/// op90's lock error.
fn download_from_host(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    header: &Header,
    txn: &[u8],
    max_return: u32,
) -> Result<Vec<u8>, PerformError> {
    let request = parse_download_request(txn)?;
    let reply = download_reply(shared, peer, session, header, &request, max_return)?;
    Ok(reply.to_unlocked_result_within(max_return as usize))
}

/// The answer to an op90 or op15 request, paged when it does not fit one
/// reply.
fn download_reply(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    header: &Header,
    request: &ReceivedDownload,
    max_return: u32,
) -> Result<DownloadReply, Refused> {
    let limit = page_limit(max_return);
    let owner = Owner::of(header);
    Ok(if request.resume.has_more() {
        match session.transfers.next(owner, &request.resume, limit)? {
            Some((page, resume)) => DownloadReply::node(page, resume),
            None => DownloadReply {
                resume: unknown_cursor(peer, &request.resume),
                ..DownloadReply::missing()
            },
        }
    } else {
        match readable(shared, peer, session, header, &request.key_path) {
            None => DownloadReply::missing().with_lock_error(ACCESS_DENIED),
            Some(file) => match read_node(file, &request.key_path) {
                None => DownloadReply::missing(),
                // A key written as a leaf holds a value, not a list.
                Some(node) if !node.push && node.children.is_empty() => {
                    let value = node.value.unwrap_or_default();
                    if value.len() > i16::MAX as usize {
                        warn!(
                            "{peer}: value at {:02x?} too large to inline",
                            request.key_path
                        );
                        DownloadReply::missing()
                    } else {
                        DownloadReply::terminal(value.to_vec())
                    }
                }
                Some(node) => {
                    let stream = shared
                        .download_overrides
                        .read()
                        .expect("download overrides lock")
                        .iter()
                        .find(|reply| reply.path == request.key_path)
                        .map_or_else(
                            || node.to_wire().unwrap_or_default(),
                            |reply| reply.slots.clone(),
                        );
                    let (page, resume) = session
                        .transfers
                        .start(owner, stream, limit, || shared.handle());
                    DownloadReply::node(page, resume)
                }
            },
        }
    })
}

/// op92 Download List: the parent node restricted to the requested children
/// that exist, in the Download wire form, paged like op90; the key list may
/// itself arrive in parts (spec 5.6.6b, 5.6.6c).
fn download_list(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    header: &Header,
    txn: &[u8],
    max_return: u32,
) -> Result<Vec<u8>, PerformError> {
    let request = parse_download_list_request(txn)?;
    let limit = page_limit(max_return);
    let owner = Owner::of(header);
    let reply = |slots, resume, status| DownloadListReply {
        status,
        slots,
        resume,
    };
    // A continuation of a download in progress.
    if request.resume.has_more() && request.resume.depth >= 0 {
        let reply = match session.transfers.next(owner, &request.resume, limit)? {
            Some((page, resume)) => reply(page, resume, 0),
            None => reply(Vec::new(), unknown_cursor(peer, &request.resume), 0),
        };
        return Ok(reply.to_result_within(max_return as usize));
    }
    let path = &request.key_path;
    // Each part of the key list is held only for a path the session may read.
    let Some(file) = readable(shared, peer, session, header, path) else {
        let reply = reply(Vec::new(), Resume::complete(), ACCESS_DENIED);
        return Ok(reply.to_result_within(max_return as usize));
    };
    let max = if file.account.is_some() {
        MAX_UPLOAD
    } else {
        MAX_PRE_LOGIN_UPLOAD
    };
    // The first part of the key list, or a later one.
    let part = request.payload;
    let upload = session
        .transfers
        .upload(owner, &request.resume, part, max, || shared.handle());
    // The key list's charge, if it arrived in parts, lasts as long as it.
    let (payload, _held) = match upload? {
        Upload::Complete(payload, held) => (payload, held),
        Upload::Partial(resume) => {
            return Ok(reply(Vec::new(), resume, 0).to_result_within(max_return as usize));
        }
        Upload::Unknown => {
            let reply = reply(Vec::new(), unknown_cursor(peer, &request.resume), 0);
            return Ok(reply.to_result_within(max_return as usize));
        }
    };
    // Checked before the store is held; read again from the bytes there.
    let keys = KeyList::parse(&payload)?;
    let file = readable(shared, peer, session, header, path).expect("checked above");
    let stream = match read_selected_node(shared, file, path, &keys) {
        Some(parent) => parent.to_wire().unwrap_or_default(),
        None => vec![om_hbam::slot::BEGIN, om_hbam::slot::POP],
    };
    let (page, resume) = session
        .transfers
        .start(owner, stream, limit, || shared.handle());
    Ok(reply(page, resume, 0).to_result_within(max_return as usize))
}

/// op86 QueryProgram: run the program's finds, joins and found-set operations
/// against the file's indexes (`om-query`) and return its result spots,
/// paged like a download; the program may arrive in parts (spec
/// query-program.md, network-protocol.md 5.6.6c).
fn query(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    header: &Header,
    txn: &[u8],
    max_return: u32,
) -> Result<Vec<u8>, PerformError> {
    let request = parse_query_request(txn)?;
    let limit = page_limit(max_return);
    let owner = Owner::of(header);
    let reply = |slots, resume| QueryReply { slots, resume };
    if request.resume.has_more() && request.resume.depth >= 0 {
        let reply = match session.transfers.next(owner, &request.resume, limit)? {
            Some((page, resume)) => reply(page, resume),
            None => reply(Vec::new(), unknown_cursor(peer, &request.resume)),
        };
        return Ok(reply.to_result_within(max_return as usize));
    }
    // The program, or this part of it, is held only for a session logged in
    // to the file; otherwise the query is refused below without it.
    let logged_in = session
        .open
        .get(&header.file_id)
        .is_some_and(|file| file.account.is_some());
    // A complete program is parsed once, here, before the file's store is
    // held, and its bytes are dropped once its tree is read (#325).
    let mut reading = None;
    let parsed = if !logged_in {
        None
    } else {
        let part = request.program;
        let upload = session
            .transfers
            .upload(owner, &request.resume, part, MAX_UPLOAD, || shared.handle());
        // The program's charge, if it arrived in parts, lasts as long as it.
        let (program, _held) = match upload? {
            Upload::Complete(program, held) => (program, held),
            Upload::Partial(resume) => {
                return Ok(reply(Vec::new(), resume).to_result_within(max_return as usize));
            }
            Upload::Unknown => {
                let reply = reply(Vec::new(), unknown_cursor(peer, &request.resume));
                return Ok(reply.to_result_within(max_return as usize));
            }
        };
        // Its tree is charged until the query is answered.
        let charge = reserve_tree(shared, session, program.len());
        let parsed = om_query::Program::parse_with_memory_limit(&program, charge.bytes());
        if matches!(parsed, Err(om_query::QueryError::Limit("program memory"))) {
            warn!("{peer}: refusing a query program past what the client may hold");
            return Err(Refused.into());
        }
        reading = Some(charge);
        Some(parsed)
    };
    let slots = match (session.open.get(&header.file_id), parsed) {
        (Some(file), Some(program))
            if file.account.is_some()
                && !(query_changes_file(&program)
                    && (file.hosted.is_read_only() || !full_access(file))) =>
        {
            let context = QueryContext {
                file_id: file.hosted.id,
                connection_key: Some(session.key),
            };
            let outcome = {
                let store = file.hosted.read_store();
                if store.is_read_only() && query_changes_file(&program) {
                    return Err(Refused.into());
                }
                let restricted = !full_access(file);
                let source = QuerySource {
                    store: &store,
                    restricted,
                    account_key: file
                        .account
                        .as_deref()
                        .map(|account| account.key.as_slice()),
                    late: shared.late_index_builds.load(Ordering::Relaxed),
                    view: view_filter(file, &store),
                    built: Default::default(),
                };
                let outcome = om_query::execute_parsed(&source, program, &context);
                // A query refused for an index this server cannot key
                // (#572) keeps none of the indexes it built of other fields.
                if matches!(outcome.error, Some(om_query::QueryError::UnkeyedIndex)) {
                    store.forget_indexes_on_demand(&source.built.borrow());
                }
                if let Some(view) = &source.view {
                    note_withheld(file, view);
                }
                outcome
            };
            if let Some(error) = &outcome.error {
                info!("{peer}: query on {:?} failed: {error}", file.hosted.name);
            }
            // Manage Database's instructions change the file,
            // GenerateMemberKey's issued ids go into their catalogs
            // (schema-path.md 3.4), and indexes the query built on demand are
            // stored (query-program.md 7.1): those of other queries too, as
            // the engine completes the draft (`engine`, "Drafts").
            let pending = {
                let store = file.hosted.read_store();
                store.has_issued_member_keys() || store.has_indexes_on_demand()
            };
            if pending || !outcome.writes.is_empty() {
                for write in &outcome.writes {
                    let (done, path) = match write {
                        om_query::Write::Put { path, .. } => ("stored", path),
                        om_query::Write::Remove { path } => ("removed", path),
                    };
                    info!("{peer}: {:?}: {done} {path:02x?}", file.hosted.name);
                }
                let draft = Draft::ChangeSchema {
                    writes: outcome.writes,
                };
                match shared
                    .engine
                    .submit_draft(&file.hosted, draft)
                    .outcome
                    .reply
                {
                    Reply::ChangeSchema(Ok(())) | Reply::StoreIndexes => {}
                    Reply::ChangeSchema(Err(_)) => return Err(Refused.into()),
                    other => return Err(mismatched(&other)),
                }
            }
            outcome.slots
        }
        // Not open or not logged in: the error slot with "access denied".
        _ => {
            let code = ACCESS_DENIED.to_be_bytes();
            vec![0x80, 0x06, 0x00, 0x03, 0x02, code[0], code[1], 0x40]
        }
    };
    // The program's tree went with the query, and its charge goes now.
    drop(reading);
    let mut slots = slots;
    if shared.unterminated_query_replies.load(Ordering::Relaxed) && slots.last() == Some(&0x40) {
        slots.pop();
    }
    let (page, resume) = session
        .transfers
        .start(owner, slots, limit, || shared.handle());
    Ok(reply(page, resume).to_result_within(max_return as usize))
}

/// Reject schema writes before execution: member-key reservation changes the
/// store during evaluation, before an Outcome's writes can be inspected.
fn query_changes_file<E>(program: &Result<om_query::Program, E>) -> bool {
    use om_query::program::opcode;
    program.as_ref().is_ok_and(|program| {
        program.instructions.iter().any(|instruction| {
            matches!(
                instruction.opcode,
                opcode::UPLOAD_MULTIPLE | opcode::GENERATE_MEMBER_KEY | opcode::DELETE_TABLE
            )
        })
    })
}

/// A hosted file as a query source. Restricted accounts see account-member
/// parents, other whole members and the `04` and `06` children as missing;
/// their own whole member is filtered to known metadata. The optional
/// late-index behavior mirrors FileMaker Server's missing-index finder
/// behavior (query-program.md 7.1).
struct QuerySource<'s> {
    store: &'s crate::store::Store,
    restricted: bool,
    account_key: Option<&'s [u8]>,
    late: bool,
    /// The account's view limits: records it may not view are read without
    /// their values, and a view-limited table has no index.
    view: Option<crate::views::ViewFilter>,
    /// The indexes this query had built on demand, which a refused query
    /// takes back ([`crate::store::Store::forget_indexes_on_demand`]).
    built: std::cell::RefCell<Vec<crate::store::IndexId>>,
}

impl om_query::Source for QuerySource<'_> {
    fn catalog(&self) -> om_hbam::Result<om_hbam::Catalog> {
        om_query::Source::catalog(self.store)
    }

    fn node(&self, path: &[&[u8]]) -> om_hbam::Result<Option<std::borrow::Cow<'_, OwnedNode>>> {
        let own = self
            .account_key
            .is_some_and(|key| self.restricted && account_member_path(path.iter().copied(), key));
        if self.restricted && credential_path(path.iter().copied()) && !own {
            return Ok(None);
        }
        let node = om_query::Source::node(self.store, path)?;
        let node = if own {
            node.map(|node| {
                let mut node = node.into_owned();
                node.children
                    .retain(|child| account_metadata(&child.segment));
                std::borrow::Cow::Owned(node)
            })
        } else {
            node
        };
        Ok(match (&self.view, node) {
            (Some(view), Some(node)) => view
                .filter(self.store, path, node.into_owned())
                .map(std::borrow::Cow::Owned),
            (_, node) => node,
        })
    }

    fn index_on_demand(
        &self,
        table: &[u8],
        field: &[u8],
        kind: u8,
    ) -> Result<Option<OwnedNode>, om_query::QueryError> {
        if self.view.as_ref().is_some_and(|view| view.limits(table)) {
            return Ok(None);
        }
        let index = om_query::Source::index_on_demand(self.store, table, field, kind)?;
        if index.is_some() {
            self.built
                .borrow_mut()
                .push((table.to_vec(), field.to_vec(), kind));
        }
        Ok(index)
    }

    fn finders_read_missing_indexes(&self) -> bool {
        self.late
    }

    fn generate_member_key(&self, catalog: &[Vec<u8>]) -> Option<(Vec<u8>, Vec<u8>)> {
        om_query::Source::generate_member_key(self.store, catalog)
    }
}

/// op63 Request Notification: register the connection for the file's change
/// notifications (write-path.md 6.1).
fn request_notification(
    peer: &str,
    session: &Session,
    header: &Header,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    let request = parse_request_notification(txn)?;
    let Some(file) = session
        .open
        .get(&header.file_id)
        .filter(|f| f.account.is_some())
    else {
        debug!(
            "{peer}: notification request for unopened file {}",
            request.file_id
        );
        return Ok(ResultBuilder::new().push_u16(ACCESS_DENIED).finish());
    };
    let registration = Registration {
        connection: session.connection_id,
        address: request.message_address,
        mask: request.attention_mask,
        client_file: request.client_file_id,
    };
    let mut registrations = file.hosted.registrations();
    registrations.retain(|r| r.connection != registration.connection);
    registrations.push(registration);
    Ok(ResultBuilder::new().push_u16(0).finish())
}

/// The open file a write names, if this session is logged in to it and
/// the file may be written (a local file that cannot be is refused here,
/// before the write changes anything, `Hosted::read_only`).
fn writable<'s>(peer: &str, session: &'s Session, header: &Header) -> Option<&'s OpenFile> {
    let file = session.open.get(&header.file_id)?;
    if set_access(file).is_none() {
        info!("{peer}: refusing a write before Login");
        return None;
    }
    if file.hosted.is_read_only() {
        info!(
            "{peer}: refusing a write to read-only {:?}",
            file.hosted.name
        );
        return None;
    }
    if views_records_only(file) {
        info!(
            "{peer}: refusing a record write to {:?} under [Read-Only Access]",
            file.hosted.name
        );
        return None;
    }
    Some(file)
}

/// The privilege set `file`'s session logged in with, as it was then
/// (`OpenFile::account`); none before Login. An account whose set cannot
/// be read is never admitted, and every check below fails closed without
/// a set.
fn set_access(file: &OpenFile) -> Option<&SetAccess> {
    file.account.as_deref()?.access.as_ref()
}

/// Whether `file`'s session logged in with the file's verified built-in
/// [Full Access] set.
fn full_access(file: &OpenFile) -> bool {
    file.account
        .as_deref()
        .is_some_and(FileAccount::full_access)
}

/// Whether the privilege set of `file`'s session lets it only view
/// records: [Read-Only Access] (session-identity.md 2.1), or no set.
fn views_records_only(file: &OpenFile) -> bool {
    set_access(file).is_none_or(|access| !access.edits_any_record())
}

/// Whether `file`'s session may take `action` on some record of base table
/// `table`: an action limited by a formula too, which the commit decides
/// (`SetAccess::may_ever`). A new record's id and serials are reserved on
/// this.
fn may_ever(file: &OpenFile, table: &[u8], action: RecordAction) -> bool {
    set_access(file).is_some_and(|access| access.may_ever(table, action))
}

/// Whether `file`'s session may view every record of every table: not
/// when its custom set limits viewing a table's records by a calculation.
/// A container upload's deduplication and a reference count answer from
/// the libraries, which hold any record's data (whether some content
/// exists, how often it is used), so such an account changes no container
/// data.
fn views_every_record(file: &OpenFile) -> bool {
    set_access(file).is_some_and(|access| !access.limits_viewing())
}

/// Whether `file`'s session may change the reference count of the
/// container member at `path`: an edit of the records that hold it, those
/// of its table, or of any table for a member of the file's own library.
fn container_member_editable(file: &OpenFile, path: &[Vec<u8>]) -> bool {
    let Some(access) = set_access(file) else {
        return false;
    };
    match crate::library::member_path(path) {
        Some((Some(table), _)) => access.may(table, RecordAction::Edit),
        Some((None, _)) => access.may_in_every_table(RecordAction::Edit),
        // Not a member: the adjustment itself refuses it.
        None => true,
    }
}

/// Whether `file`'s session may store container data in the library op88
/// names (`table`): a create or an edit of that table's records, which a
/// container cell naming the data is; for the file's own library, which
/// any table's records may hold, one of the two in every base table of
/// the file, each by its own rules (the set's default counts only for a
/// table without an entry). Data already stored raises that member's count
/// instead, which op87 allows only to such an edit
/// ([`container_member_editable`]). FileMaker Server's answer to an upload
/// its set denies is unmeasured; it is refused as op88's other privilege
/// refusals are. A request that names no table library is refused by the
/// upload itself.
fn container_library_writable(file: &OpenFile, table: &[Vec<u8>]) -> bool {
    let Some(access) = set_access(file) else {
        return false;
    };
    let actions = [RecordAction::Create, RecordAction::Edit];
    match table {
        [] => {
            let tables: Vec<Vec<u8>> = file
                .hosted
                .read_store()
                .catalog()
                .tables
                .iter()
                .map(|t| om_hbam::slot::uint_segment(t.id))
                .collect();
            access.may_any_in_tables(tables.iter().map(Vec::as_slice), &actions)
        }
        [table] => actions.iter().any(|a| access.may(table, *a)),
        _ => true,
    }
}

/// The lock user for a transaction and the name other users see for it:
/// "session name (account)" (write-path.md 4.2).
fn lock_user(session: &Session, file: &OpenFile, txn: u32) -> (LockUser, String) {
    let user = LockUser {
        connection: session.connection_id,
        txn,
    };
    let account = file.account.as_deref().map_or("?", |account| &account.name);
    (user, format!("{} ({account})", session.name))
}

/// The locks `session` may still take ([`writes::MAX_LOCKS`]): the cap
/// less what it holds in all its files.
fn lock_room(session: &Session) -> usize {
    let held: usize = session
        .open
        .values()
        .map(|file| file.hosted.lock_table().held_by(session.connection_id))
        .sum();
    writes::MAX_LOCKS.saturating_sub(held)
}

/// Whether this op71 is the one a test has refused
/// ([`crate::Server::refusing_create_after`]). Only in debug builds.
#[cfg(debug_assertions)]
fn create_refused_for_test(shared: &Shared) -> bool {
    shared.test_refusal.get().is_some_and(TestPanic::due)
}

#[cfg(not(debug_assertions))]
fn create_refused_for_test(_: &Shared) -> bool {
    false
}

/// op71 HostCreateRecord: reserve record ids (write-path.md 3.1). The new
/// last issued id is saved before the ids are answered
/// ([`writes::create_record`]).
fn create_record(
    shared: &Shared,
    peer: &str,
    session: &Session,
    header: &Header,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    let request = write::parse_create_record_request(txn)?;
    let Some(file) = writable(peer, session, header) else {
        return Ok(write::CreateRecordReply {
            conflict: None,
            status: u32::from(ACCESS_DENIED),
            record: vec![vec![0x00]],
        }
        .to_result());
    };
    let table = request.table.last().map(Vec::as_slice).unwrap_or_default();
    if !may_ever(file, table, RecordAction::Create) {
        info!("{peer}: refusing a new record its privilege set does not allow");
        return Ok(write::CreateRecordReply {
            conflict: None,
            status: u32::from(ACCESS_DENIED),
            record: vec![vec![0x00]],
        }
        .to_result());
    }
    let (user, holder) = lock_user(session, file, request.txn);
    let room = lock_room(session);
    // A client's reservation is on disk before it is answered (#927).
    let reserve = ReserveRecordIds {
        user,
        holder,
        request,
        room,
        durable: true,
    };
    let outcome = shared
        .engine
        .submit(&file.hosted, Operation::ReserveRecordIds(reserve));
    match outcome.reply {
        Reply::ReserveRecordIds(reply) => Ok(reply.to_result()),
        other => Err(mismatched(&other)),
    }
}

/// An engine's reply of another operation than the one submitted, which
/// the engine never gives.
fn mismatched(reply: &Reply) -> PerformError {
    PerformError::Status(format!(
        "the engine answered with another operation's reply: {reply:?}"
    ))
}

/// op72 HostGetSerial: a field's next serial value (write-path.md 3.2).
fn get_serial(
    shared: &Shared,
    session: &Session,
    header: &Header,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    let request = write::parse_get_serial_request(txn)?;
    let reply = match session
        .open
        .get(&header.file_id)
        // Reserving a value and Set Next Serial Value both save the field:
        // a record write, refused to [Read-Only Access] as `writable` does.
        .filter(|f| f.account.is_some() && !f.hosted.is_read_only() && !views_records_only(f))
        // A serial is taken for a new record of its table.
        .filter(|f| {
            may_ever(
                f,
                request.table.last().map(Vec::as_slice).unwrap_or_default(),
                RecordAction::Create,
            )
        }) {
        Some(file) => {
            let advance = AdvanceSerial { request };
            let outcome = shared
                .engine
                .submit(&file.hosted, Operation::AdvanceSerial(advance));
            match outcome.reply {
                Reply::AdvanceSerial(reply) => reply,
                other => return Err(mismatched(&other)),
            }
        }
        None => write::GetSerialReply {
            conflict: None,
            status: u32::from(ACCESS_DENIED),
            value: String::new(),
        },
    };
    Ok(reply.to_result())
}

/// The complete payload of an upload-style request, with the charge its
/// bytes hold if they arrived in parts (keep it as long as the payload), or
/// the resume block to answer with while its parts are still arriving (spec
/// 5.6.6c). The caller has authorized the request: each part is held only
/// for one that may run.
fn upload_payload(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    header: &Header,
    resume: &Resume,
    part: Vec<u8>,
) -> Result<Result<(Vec<u8>, Charge), Resume>, Refused> {
    let owner = Owner::of(header);
    let upload = session
        .transfers
        .upload(owner, resume, part, MAX_UPLOAD, || shared.handle());
    Ok(match upload? {
        Upload::Complete(payload, held) => Ok((payload, held)),
        Upload::Partial(resume) => Err(resume),
        Upload::Unknown => Err(unknown_cursor(peer, resume)),
    })
}

/// op14 Upload List To Host Temp Location: stage records for a commit
/// (write-path.md 3.3); op13 Upload To Host, the same with the
/// temporary-area flag clear, writes into the file.
fn upload(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    header: &Header,
    txn: &[u8],
    max_return: u32,
) -> Result<Vec<u8>, PerformError> {
    let mut request = write::parse_upload_request(txn)?;
    let done = write::upload_reply(&Resume::no_stream(), max_return as usize);
    // Each part is held only for a request that may write. A later part
    // for a file the session may no longer write continues an upload that
    // was dropped with its session in the file (closed, logged out of): it
    // fails, as a dropped transfer does, rather than answer that the upload
    // completed (the reply has no status to carry an error, spec 3.3).
    let Some(file) = writable(peer, session, header) else {
        if request.resume.has_more() {
            return Err(Refused.into());
        }
        return Ok(done);
    };
    let to_file = request.flags & write::UPLOAD_TO_TEMP == 0;
    // Native uploads can replace a script through any catalog ancestor or
    // descendant. Apply the same account boundary as opcode 240 to every
    // direct catalog upload; staged record edits stay separate.
    if to_file && !full_access(file) {
        info!("{peer}: refusing a catalog upload without Full Access");
        return Ok(done);
    }
    let hosted = file.hosted.clone();
    let part = std::mem::take(&mut request.payload);
    let (payload, _held) =
        match upload_payload(shared, peer, session, header, &request.resume, part)? {
            Ok(payload) => payload,
            Err(resume) => return Ok(write::upload_reply(&resume, max_return as usize)),
        };
    let mut reading = reserve_tree(shared, session, payload.len());
    if to_file {
        upload_to_file(shared, peer, &hosted, &request, &payload, reading.bytes())?;
        return Ok(done);
    }
    let client = session.client;
    let temp = session
        .temp
        .entry(header.file_id)
        .or_insert_with(|| writes::TempArea::new(Charge::none(&shared.upload_memory, client)));
    match writes::stage_upload(temp, &request.dest, &payload, &mut reading) {
        Ok(records) => debug!("{peer}: staged {records} record(s)"),
        Err(writes::StageError::Malformed(e)) => warn!("{peer}: {e}"),
        Err(writes::StageError::Full) => {
            warn!("{peer}: refusing to stage records past what the client may hold");
            return Err(Refused.into());
        }
    }
    Ok(done)
}

/// Reserve, before an upload of `len` bytes is read into a tree, the most
/// memory the read may take ([`uploads::tree_bound`]), or what the client
/// may still hold if that is less ([`crate::uploads`]). The read is then
/// limited to what was reserved, so no tree grows past what is charged for
/// it; the charge is given back when it is dropped.
fn reserve_tree(shared: &Shared, session: &Session, len: usize) -> Charge {
    reserve_up_to(shared, session, uploads::tree_bound(len))
}

/// Reserve `most` bytes, or what the client may still hold if that is less
/// ([`crate::uploads`]).
fn reserve_up_to(shared: &Shared, session: &Session, most: usize) -> Charge {
    let memory = &shared.upload_memory;
    let bytes = most.min(memory.available(session.client));
    // Another client may reserve between the two: then nothing, and the
    // read is refused.
    memory
        .reserve(session.client, bytes)
        .unwrap_or_else(|| Charge::none(memory, session.client))
}

/// op13 Upload To Host: put an uploaded subtree straight into the file at
/// its destination, replacing what was there (schema-path.md 3.3). Manage
/// Database saves each catalog's index this way.
fn upload_to_file(
    shared: &Shared,
    peer: &str,
    hosted: &Hosted,
    request: &write::Upload,
    payload: &[u8],
    room: usize,
) -> Result<(), PerformError> {
    if request.dest.is_empty() {
        warn!("{peer}: refusing an upload over the whole file");
        return Ok(());
    }
    let node = if request.flags & !write::UPLOAD_TO_TEMP == write::UPLOAD_VALUE {
        OwnedNode::leaf(Vec::new(), request.terminal.clone())
    } else {
        match writes::read_upload(payload, room) {
            Ok(tree) => OwnedNode { push: true, ..tree },
            Err(writes::StageError::Malformed(e)) => {
                warn!(
                    "{peer}: upload to {:02x?} is not a slot list: {e}",
                    request.dest
                );
                return Ok(());
            }
            Err(writes::StageError::Full) => {
                warn!("{peer}: refusing to read an upload past the connection's limit");
                return Err(Refused.into());
            }
        }
    };
    info!(
        "{peer}: {:?}: stored {:02x?} ({} bytes)",
        hosted.name,
        request.dest,
        payload.len().max(request.terminal.len())
    );
    let upload = UploadToCatalog {
        path: request.dest.clone(),
        node,
    };
    match shared
        .engine
        .submit(hosted, Operation::UploadToCatalog(upload))
        .reply
    {
        Reply::UploadToCatalog(_) => Ok(()),
        other => Err(mismatched(&other)),
    }
}

/// op73 HostUpdateTable: do the field work the client listed for a table
/// after saving its fields, recomputing the stored calculations the change
/// made stale (schema-path.md 3.6, 7.2). The reply is empty; the other
/// clients learn of the new values when the client releases the table's
/// lock (schema-path.md 9), which it does after this reply.
fn update_table(
    shared: &Shared,
    peer: &str,
    session: &Session,
    header: &Header,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    let table = schema::parse_update_table_request(txn)?;
    if let (Some(file), [table]) = (writable(peer, session, header), table.as_slice()) {
        let account = file.account.as_deref().map_or("", |account| &account.name);
        let update = UpdateTable {
            table: table.clone(),
            account: account.to_owned(),
        };
        match shared
            .engine
            .submit(&file.hosted, Operation::UpdateTable(update))
            .reply
        {
            Reply::UpdateTable(Ok(_)) => {}
            Reply::UpdateTable(Err(reason)) => return Err(PerformError::Status(reason.into())),
            other => return Err(mismatched(&other)),
        }
    }
    Ok(schema::update_table_reply())
}

/// op61 Upgrade: change a lock's type (schema-path.md 4.2); answered like
/// op60.
fn upgrade(
    shared: &Shared,
    session: &Session,
    header: &Header,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    let request = schema::parse_upgrade_request(txn)?;
    let Some(file) = session
        .open
        .get(&header.file_id)
        .filter(|f| f.account.is_some())
    else {
        return Ok(write::LockReply::Refused {
            status: ACCESS_DENIED,
            conflict: write::Conflict {
                self_conflict: false,
                users: String::new(),
            },
        }
        .to_result()?);
    };
    let (user, holder) = lock_user(session, file, request.txn);
    let room = lock_room(session);
    let upgrade = UpgradeLock {
        user,
        holder,
        request,
        room,
    };
    let outcome = shared
        .engine
        .submit(&file.hosted, Operation::UpgradeLock(upgrade));
    match outcome.reply {
        Reply::UpgradeLock(reply) => Ok(reply.to_result()?),
        other => Err(mismatched(&other)),
    }
}

/// The host's local date and time as FileMaker's date-time key: the date
/// and time keys of a timestamp index (schema-path.md 3.1).
pub(crate) fn host_time_key() -> Vec<u8> {
    let now = jiff::Zoned::now();
    date_time_key(
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
    )
}

fn date_time_key(year: i16, month: i8, day: i8, hour: i8, minute: i8, second: i8) -> Vec<u8> {
    let text = format!("{month}/{day}/{year} {hour}:{minute:02}:{second:02}");
    om_collate::timestamp_key(&text, om_collate::DateOrder::MonthDayYear)
        .expect("a valid date and time")
}

/// op70 HostCommit: write staged records into the file and notify the other
/// clients (write-path.md 3.4, 5, 6).
fn commit(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    header: &Header,
    txn: &[u8],
    max_return: u32,
) -> Result<Vec<u8>, PerformError> {
    let request = write::parse_commit_request(txn)?;
    let done = write::upload_reply(&Resume::no_stream(), max_return as usize);
    // Each part is held only for a request that may write.
    let Some(file) = writable(peer, session, header) else {
        return Ok(done);
    };
    let hosted = file.hosted.clone();
    // `writable` admits only a session with a set.
    let Some(account) = file.account.clone() else {
        return Ok(done);
    };
    let (user, _) = lock_user(session, file, request.txn);
    let (list, held) =
        match upload_payload(shared, peer, session, header, &request.resume, request.list)? {
            Ok(list) => list,
            Err(resume) => return Ok(write::upload_reply(&resume, max_return as usize)),
        };
    // The list's tree is charged until the commit is done.
    let reading = reserve_tree(shared, session, list.len());
    let list = match writes::read_upload(&list, reading.bytes()) {
        Ok(list) => list,
        Err(writes::StageError::Malformed(e)) => {
            warn!("{peer}: commit list is not a slot list: {e}");
            return Ok(done);
        }
        Err(writes::StageError::Full) => {
            warn!("{peer}: refusing to read a commit list past what the client may hold");
            return Err(Refused.into());
        }
    };
    // `writable` admits only a session with a set.
    let Some(access) = account.access.clone() else {
        warn!("{peer}: commit refused: the account has no privilege set");
        return Ok(done);
    };
    // The records of the list this session was served without their values
    // (`writes::refused_action`).
    let withheld = session
        .open
        .get(&header.file_id)
        .map(|file| {
            file.withheld
                .lock()
                .expect("withheld records lock")
                .named_in(&list)
        })
        .unwrap_or_default();
    let client = session.client;
    let temp = session
        .temp
        .entry(header.file_id)
        .or_insert_with(|| writes::TempArea::new(Charge::none(&shared.upload_memory, client)));
    let (staged, lent) = temp.lend(&list);
    // The account's set decides each record's action under the write lock
    // (`writes::refused_action`); a refused commit writes nothing and the
    // reply cannot say so (the client checks first and reports 200).
    // Saved by the engine; a commit that could not be saved was taken back
    // and fails with a system exception (MARSHAL, COMPLETED_NO) rather than
    // the page that says it was done: op70's reply has no status
    // (write-path.md 3.4). OpenMaker's choice; FileMaker Server's answer is
    // unobserved.
    let commit = CommitRecords {
        user,
        account: account.name.clone(),
        access,
        list,
        staged,
        withheld,
    };
    let outcome = shared
        .engine
        .submit(&hosted, Operation::CommitRecords(commit));
    let committed = match outcome.reply {
        Reply::CommitRecords(committed) => committed,
        other => {
            temp.settle(lent, Default::default());
            return Err(mismatched(&other));
        }
    };
    temp.settle(lent, committed.unused);
    let reply = match committed.result {
        Ok(()) => {
            outcome.notices.deliver(&hosted, &shared.mailboxes);
            Ok(done)
        }
        Err(writes::CommitError::Refused(e)) => {
            warn!("{peer}: {e}");
            Ok(done)
        }
        Err(writes::CommitError::NotSaved(e)) => {
            warn!("{peer}: {e}");
            Err(PerformError::Status("commit not saved".into()))
        }
    };
    drop((reading, held));
    reply
}

/// op30 Perform Script on Server: run a script of the file in a server
/// script session with the session's account and its privileges
/// (script-runtime.md 11.3).
/// The file must be open and logged in to in this session (100 otherwise).
fn perform_script(
    shared: &Arc<Shared>,
    peer: &str,
    session: &Session,
    header: &Header,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    // A request without the callback ids (from a client that has not logged
    // in, which names no protocol version) is read as such.
    let request = script::parse_perform_script_request(txn, PROTOCOL_VERSION)
        .or_else(|_| script::parse_perform_script_request(txn, PROTOCOL_VERSION - 1))?;
    let Some((file, account)) = session
        .open
        .get(&header.file_id)
        .and_then(|f| Some((f, f.account.clone()?)))
    else {
        info!("{peer}: Perform Script on Server for a file not open in the session");
        return Ok(scripts::not_open().to_result());
    };
    info!(
        "{peer}: Perform Script on Server {:02x?} in {:?} ({})",
        request.script,
        file.hosted.name,
        if request.waits() {
            "waiting"
        } else {
            "not waiting"
        }
    );
    // A new script is admitted only while the file is open or paused, so
    // that a closing file drains (`ServerHandle::close_file`); one that is
    // closing answers as a file not open (OpenMaker's choice).
    let admitted = shared
        .hosting
        .slot(file.hosted.id)
        .ok_or(FileStatus::Closed)
        .and_then(|slot| slot.admit(Arc::clone));
    let Ok(hosted) = admitted else {
        info!(
            "{peer}: Perform Script on Server refused: {:?} is closing",
            file.hosted.name
        );
        return Ok(scripts::not_open().to_result());
    };
    #[cfg(debug_assertions)]
    if let Some(gate) = shared.script_gate.get() {
        gate.hold();
    }
    let reply = scripts::perform(
        shared,
        scripts::Request {
            hosted,
            privileges: account.privileges(),
            access: account.access.clone(),
            account: account.name.clone(),
            account_key: account.key.clone(),
            requester: session.connection_id,
            script: request,
        },
    );
    Ok(reply.to_result())
}

/// op88 UploadBinaryData: store container data in a table's library
/// (containers.md 3).
fn upload_binary(
    shared: &Shared,
    peer: &str,
    session: &mut Session,
    header: &Header,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    let mut request = container::parse_upload_binary_request(txn)?;
    let failed = |status| containers::upload_reply(status, Vec::new(), String::new()).to_result();
    // Each part is held only for a request that may write.
    if !session.open.contains_key(&header.file_id) {
        return Ok(failed(FILE_NOT_OPEN));
    }
    let Some(file) = writable(peer, session, header) else {
        return Ok(failed(ACCESS_DENIED));
    };
    if !views_every_record(file) {
        info!("{peer}: refusing container data from an account with a view limit");
        return Ok(failed(ACCESS_DENIED));
    }
    if !container_library_writable(file, &request.table) {
        info!(
            "{peer}: refusing container data for a table its privilege set does not let it write"
        );
        return Ok(failed(ACCESS_DENIED));
    }
    let hosted = file.hosted.clone();
    let part = std::mem::take(&mut request.payload);
    let (payload, held) =
        match upload_payload(shared, peer, session, header, &request.resume, part)? {
            Ok(payload) => payload,
            Err(resume) => {
                // An intermediate part: no member yet (spec 3.3).
                let reply = UploadBinaryReply {
                    resume,
                    error: String::new(),
                    member: Vec::new(),
                    status: container::UPLOAD_CONTINUES,
                };
                return Ok(reply.to_result());
            }
        };
    // The tree is read within what the client may still hold beside the
    // held payload, up to its own limit (`library::tree_limit`); a tree past
    // a reservation the client's share cut short is refused as over the
    // budget, never as bad data.
    let reading = reserve_up_to(shared, session, crate::library::tree_limit(payload.len()));
    let reply =
        containers::upload_binary(&hosted, &request.table, &payload, reading.bytes(), || {
            shared.random()
        });
    drop((reading, payload, held));
    let Some(reply) = reply else {
        warn!("{peer}: refusing container data past what the client may hold");
        return Err(Refused.into());
    };
    hosted.save();
    Ok(reply.to_result())
}

/// op87 AdjustReferenceCount: change a container member's reference count
/// (containers.md 5.3).
fn adjust_reference_count(
    peer: &str,
    session: &Session,
    header: &Header,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    let request = container::parse_adjust_reference_count_request(txn)?;
    let count = match writable(peer, session, header) {
        Some(file)
            if !views_every_record(file) || !container_member_editable(file, &request.path) =>
        {
            info!("{peer}: refusing a container count its privilege set does not allow");
            container::COUNT_FAILED
        }
        Some(file) => {
            let count = containers::adjust_reference_count(&file.hosted, &request);
            file.hosted.save();
            count
        }
        None => container::COUNT_FAILED,
    };
    Ok(container::adjust_reference_count_reply(count))
}

/// op96 DownloadThumbnail: a container image scaled to a box, in chunks
/// (containers.md 6.3).
fn thumbnail(
    session: &Session,
    header: &Header,
    txn: &[u8],
    max_return: u32,
) -> Result<Vec<u8>, PerformError> {
    /// Room for the reply's other fields.
    const REPLY_OVERHEAD: usize = 64;
    let request = container::parse_download_thumbnail_request(txn)?;
    let reply = match session
        .open
        .get(&header.file_id)
        .filter(|f| f.account.is_some())
    {
        // A view-limited table's container data is withheld (its library
        // is per table, not per record).
        Some(file)
            if {
                let store = file.hosted.read_store();
                view_filter(file, &store)
                    .is_some_and(|view| request.table.last().is_none_or(|table| view.limits(table)))
            } =>
        {
            container::ThumbnailReply::Status(container::THUMBNAIL_MISSING)
        }
        Some(file) => containers::thumbnail(
            &file.hosted,
            &request,
            page_limit(max_return).saturating_sub(REPLY_OVERHEAD),
        ),
        None => container::ThumbnailReply::Status(container::THUMBNAIL_MISSING),
    };
    Ok(reply.to_result())
}

/// op60 Lock: take a record lock without waiting (write-path.md 4.2).
fn lock(
    shared: &Shared,
    session: &Session,
    header: &Header,
    txn: &[u8],
) -> Result<Vec<u8>, PerformError> {
    let request = write::parse_lock_request(txn)?;
    let Some(file) = session
        .open
        .get(&header.file_id)
        .filter(|f| f.account.is_some())
    else {
        return Ok(write::LockReply::Refused {
            status: ACCESS_DENIED,
            conflict: write::Conflict {
                self_conflict: false,
                users: String::new(),
            },
        }
        .to_result()?);
    };
    let (user, holder) = lock_user(session, file, request.txn);
    let room = lock_room(session);
    let lock = TakeLock {
        user,
        holder,
        request,
        room,
    };
    let outcome = shared
        .engine
        .submit(&file.hosted, Operation::TakeLock(lock));
    match outcome.reply {
        Reply::TakeLock(reply) => Ok(reply.to_result()?),
        other => Err(mismatched(&other)),
    }
}

/// A CORBA system exception reply (`IDL:omg.org/CORBA/<name>:1.0`).
fn exception(request_id: u32, name: &str) -> Vec<u8> {
    build_system_exception_reply(
        request_id,
        &format!("IDL:omg.org/CORBA/{name}:1.0"),
        0,
        Completion::No,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_time_is_a_timestamp_key() {
        // live:s0/r274_op82: 2026-10-28 15:12:42 (schema-path.md 3.1).
        assert_eq!(
            date_time_key(2026, 10, 28, 15, 12, 42),
            [0x9f, 0xaa, 0x9c, 0x80, 0x05, 0x65, 0x87, 0x30]
        );
        // Midnight is second 0.
        assert_eq!(date_time_key(2026, 1, 1, 0, 0, 0)[3..], [0x80, 0x00]);
        // Now: a date (high bit set), then a number key.
        let now = host_time_key();
        assert!(now[0] & 0x80 != 0 && now[3] == 0x80, "{now:02x?}");
        assert_eq!(now.len(), 5 + usize::from(now[4]).div_ceil(2));
    }
}
