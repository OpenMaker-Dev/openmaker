//! Accepting connections, one thread each, within the caps and deadlines
//! of [`ConnectionLimits`]: a connection over a cap is closed as soon as it
//! is accepted, before any TLS work or thread; its TLS handshake has one
//! deadline for the whole of it; a connection that stays silent is closed
//! (`Connection::serve`), and one whose client takes none of what the
//! server writes for too long is ended.
//!
//! A connection's thread releases what it holds however it ends, by a
//! panic in a request handler too: its session (`Connection`'s `Drop`),
//! then its place under the caps and its socket, shut down both ways so
//! that the client sees the end (`Served`). A panic costs only that
//! connection, and is logged once at error level with the client's
//! address and the panic's message.

use std::any::Any;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use log::{debug, error, info, warn};
use ring::rand::SystemRandom;
use rustls::{ServerConnection, StreamOwned};

use crate::admin::{ClientSocket, Control};
use crate::admission::{Admissions, Slot};
use crate::backups::{BackupError, BackupInfo, BackupProgress};
use crate::config::{ConnectionLimits, EngineSettings, ScriptLimits, ServerConfig};
use crate::connection::{Connection, Shared};
use crate::engine::Engine;
use crate::eventlog::{Event, EventLog};
use crate::hosting::{Hosting, HostingError};
use crate::record::Recordings;
use crate::schedules::{Clock, Schedule, ScheduleError, ScheduleStatus, Scheduler};
use crate::scripts::{AbortReason, RunningScript, ScriptEngine};
use crate::tls::{server_config, Identity, IdentityError};
use crate::transfer::DownloadMemory;
use crate::uploads::UploadMemory;
use om_calc::eval::CalcLimits;

/// Errors starting a server.
#[derive(Debug)]
pub enum StartError {
    Io(io::Error),
    Hosting(HostingError),
    Identity(IdentityError),
    /// The `--record` folder cannot be made or read.
    Record {
        dir: PathBuf,
        error: io::Error,
    },
    /// The log folder cannot be made or written.
    EventLog {
        dir: PathBuf,
        error: io::Error,
    },
    /// The store engine's folder cannot be opened (`--store-dir`).
    Store(crate::engine::store::StoreError),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Io(e) => write!(f, "{e}"),
            StartError::Hosting(e) => write!(f, "{e}"),
            StartError::Identity(e) => write!(f, "{e}"),
            StartError::Record { dir, error } => {
                write!(f, "cannot record connections in {}: {error}", dir.display())
            }
            StartError::EventLog { dir, error } => {
                write!(
                    f,
                    "cannot write the event log in {}: {error}",
                    dir.display()
                )
            }
            StartError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StartError {}

/// How long a connection waits for a client's request before it sends the
/// back-channel calls waiting for that client.
const CALL_POLL: Duration = Duration::from_millis(100);

/// How long the accept loop waits after a failed accept other than a
/// client's own abort: out of descriptors or memory, the next accept fails
/// the same way at once.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// The least time between two warnings of one kind that go on while a
/// condition lasts (refused connections, failed accepts, threads that do
/// not start).
const WARNING_INTERVAL: Duration = Duration::from_secs(60);

/// Descriptors a connection holds: its socket and the handle `Sockets`
/// keeps to close it with; recording adds its two files.
const DESCRIPTORS_PER_CONNECTION: usize = 2;
const DESCRIPTORS_PER_RECORDING: usize = 2;

/// Descriptors left free beside the connections for the server's own
/// work, with two more per hosted file: a save's new copy and its folder,
/// container files, a script's files. At most a quarter of the open-file
/// limit, so that a small limit still leaves room for some connections.
const DESCRIPTORS_FOR_WORK: usize = 64;

/// A bound OpenMaker server: hosting loaded, TLS ready, listening.
pub struct Server {
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    shared: Arc<Shared>,
    schedules: Arc<Scheduler>,
    limits: ConnectionLimits,
    admissions: Arc<Admissions>,
    /// The stack size of connection threads, if not the default.
    stack_size: Option<usize>,
    /// The database folders the server found its files in at startup
    /// ([`Server::with_database_folders`]).
    database_folders: Arc<Vec<PathBuf>>,
}

impl Server {
    /// Load the hosted files and bind `addr` (FileMaker's port is 5003). A
    /// file that cannot be opened is not hosted, and raises an alert
    /// ([`ServerHandle::alerts`]); the server does not start when none of
    /// the files opens.
    pub fn bind(
        addr: impl ToSocketAddrs,
        config: ServerConfig,
        identity: Identity,
    ) -> Result<Self, StartError> {
        Self::bind_on(addr, config, identity, &EngineSettings::File)
    }

    /// [`Server::bind`], the files hosted by the engine `settings` name
    /// (`--engine`, docs/server-storage.md 11): on the store engine, its
    /// folder is opened, and each file the store does not hold as it is on
    /// disk is imported first.
    pub fn bind_on(
        addr: impl ToSocketAddrs,
        config: ServerConfig,
        identity: Identity,
        settings: &EngineSettings,
    ) -> Result<Self, StartError> {
        let events = EventLog::new(crate::eventlog::host_name(&config.identity.name));
        let engine = match settings {
            EngineSettings::File => Engine::default(),
            EngineSettings::Store {
                dir,
                cache_bytes,
                database_folders,
            } => {
                let folder =
                    crate::engine::StoreFolder::open_dir(dir, database_folders, *cache_bytes)
                        .map_err(StartError::Store)?;
                info!("the store engine hosts the files, from {}", dir.display());
                Engine::Store(crate::engine::StoreEngine::new(Arc::new(folder)))
            }
        };
        let hosting = Hosting::load(&config.files, config.insecure_files, &events, &engine)
            .map_err(StartError::Hosting)?;
        let tls = server_config(identity).map_err(StartError::Identity)?;
        Self::start(addr, config, hosting, tls, false, events, engine)
    }

    /// Bind `addr` for `hosting`, with TLS `tls`, recording in `events`
    /// and running every change to the files with `engine`, the one that
    /// loaded them; `local`: the private host of a file a client opened
    /// from its own computer ([`crate::LocalHost`]).
    pub(crate) fn start(
        addr: impl ToSocketAddrs,
        config: ServerConfig,
        hosting: Hosting,
        tls: Arc<rustls::ServerConfig>,
        local: bool,
        events: EventLog,
        engine: Engine,
    ) -> Result<Self, StartError> {
        // A server asked to record refuses to start without its folder
        // rather than serve unrecorded.
        let recordings = config
            .record_dir
            .map(|dir| Recordings::open(&dir).map_err(|error| StartError::Record { dir, error }))
            .transpose()?;
        if let Some(recordings) = &recordings {
            info!(
                "recording connections in {} from conn-{:03}",
                recordings.dir().display(),
                recordings.next_index()
            );
        }
        for file in hosting.files() {
            info!("hosting {:?} as file {}", file.name, file.id);
        }
        let listener = TcpListener::bind(addr).map_err(StartError::Io)?;
        Ok(Self {
            listener,
            tls,
            shared: Arc::new(Shared {
                identity: config.identity,
                hosting,
                local,
                login_limits: Default::default(),
                custom_sets: AtomicBool::new(false),
                rng: SystemRandom::new(),
                connection_ids: Mutex::new(Default::default()),
                joins: Default::default(),
                recordings,
                mailboxes: Default::default(),
                engine: Arc::new(engine),
                scripts: ScriptEngine::new(ScriptLimits::DEFAULT),
                refused_downloads: RwLock::new(Vec::new()),
                omitted_list_paths: Default::default(),
                download_overrides: Default::default(),
                shutdown: AtomicBool::new(false),
                late_index_builds: AtomicBool::new(false),
                unterminated_query_replies: AtomicBool::new(false),
                started: SystemTime::now(),
                block_new_users: AtomicBool::new(false),
                clients: Default::default(),
                events,
                open_delays: RwLock::new(Default::default()),
                sockets: Default::default(),
                listed: RwLock::new(Default::default()),
                file_list_override: RwLock::new(None),
                no_file_list: AtomicBool::new(false),
                no_get_file_list: AtomicBool::new(false),
                no_authoring: AtomicBool::new(false),
                upload_memory: UploadMemory::new(ConnectionLimits::DEFAULT.upload_memory),
                download_memory: DownloadMemory::new(ConnectionLimits::DEFAULT.download_memory),
                #[cfg(debug_assertions)]
                test_panic: Default::default(),
                #[cfg(debug_assertions)]
                test_refusal: Default::default(),
                #[cfg(debug_assertions)]
                script_gate: Default::default(),
                #[cfg(debug_assertions)]
                failing_timestamps: Default::default(),
                #[cfg(debug_assertions)]
                read_only_before_write: AtomicBool::new(false),
            }),
            schedules: Scheduler::new(),
            limits: ConnectionLimits::DEFAULT,
            admissions: Default::default(),
            stack_size: None,
            database_folders: Arc::default(),
        })
    }

    /// Let at most `limit` Perform Script on Server scripts execute at once;
    /// more are refused with FileMaker's error 812 (script-runtime.md 11.3).
    pub fn with_script_limit(self, limit: usize) -> Self {
        self.shared.scripts.set_limit(limit);
        self
    }

    /// The limits of Perform Script on Server scripts: how many execute at
    /// once, and how many steps and how long one may run before it is
    /// aborted ([`ScriptLimits::DEFAULT`] otherwise).
    pub fn with_script_limits(self, limits: ScriptLimits) -> Self {
        self.shared.scripts.set_limits(limits);
        self
    }

    /// Give the threads of Perform Script on Server scripts that do not
    /// wait a stack of `bytes`. A test fixture: a stack larger than the
    /// address space makes every such thread fail to start, as when the
    /// system has no threads left.
    #[doc(hidden)]
    pub fn with_script_stack_size(self, bytes: usize) -> Self {
        self.shared.scripts.set_stack_size(bytes);
        self
    }

    /// What one calculation of a server script may build: the size of a
    /// text value and how deep custom function calls and `Evaluate`s nest
    /// ([`crate::SCRIPT_CALC_LIMITS`] otherwise).
    pub fn with_calc_limits(self, limits: CalcLimits) -> Self {
        self.shared.scripts.set_calc_limits(limits);
        self
    }

    /// Admit accounts in custom privilege sets, as a file's private host
    /// does, though this server does not yet hold a client to every member
    /// of one (`hosting::FileAccount::server_eligibility`, #734). A test
    /// fixture, for tests of what a client does with such an account.
    #[doc(hidden)]
    pub fn admitting_custom_sets(self) -> Self {
        self.shared.custom_sets.store(true, Ordering::Relaxed);
        self
    }

    /// The caps on connections and the time limits on each
    /// ([`ConnectionLimits::DEFAULT`] otherwise).
    pub fn with_connection_limits(mut self, limits: ConnectionLimits) -> Self {
        self.shared.upload_memory.set_limit(limits.upload_memory);
        self.shared
            .download_memory
            .set_limit(limits.download_memory);
        self.limits = limits;
        self
    }

    /// Give each connection's thread a stack of `bytes`. A test fixture:
    /// a stack larger than the address space makes every connection's
    /// thread fail to start, as when the system has no threads left.
    #[doc(hidden)]
    pub fn with_connection_stack_size(mut self, bytes: usize) -> Self {
        self.stack_size = Some(bytes);
        self
    }

    /// Panic answering a client's Perform of `opcode`, as a bug in its
    /// handler would; with `holding_locks`, while holding every shared
    /// lock a session's end takes. A test fixture for what a panicking
    /// connection leaves behind, only in debug builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn panicking_on(self, opcode: u8, holding_locks: bool) -> Self {
        let _ = self.shared.test_panic.set(crate::connection::TestPanic {
            opcode,
            holding_locks,
            skip: Default::default(),
            once: false,
        });
        self
    }

    /// Refuse one HostCreateRecord (op71), the one after `skip` of them
    /// were answered, with the status a privilege set that may not create
    /// gets (ACCESS_DENIED): a reservation a host refuses. Only in debug
    /// builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn refusing_create_after(self, skip: u32) -> Self {
        let _ = self.shared.test_refusal.set(crate::connection::TestPanic {
            opcode: om_protocol::Opcode::HostCreateRecord as u8,
            holding_locks: false,
            skip: skip.into(),
            once: true,
        });
        self
    }

    /// Panic answering one Perform of `opcode`, the one after `skip` of
    /// them were answered; the later ones are answered again: a request
    /// lost after the same request succeeded earlier, and before a
    /// reconnect sends it again. Only in debug builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn panicking_on_after(self, opcode: u8, skip: u32) -> Self {
        let _ = self.shared.test_panic.set(crate::connection::TestPanic {
            opcode,
            holding_locks: false,
            skip: skip.into(),
            once: true,
        });
        self
    }

    /// Hold each Perform Script on Server request between its admission to
    /// its file and its script's start until the returned gate is opened. A
    /// test fixture for a close that begins in between, only in debug
    /// builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn holding_scripts(self) -> (Self, crate::connection::ScriptGate) {
        let gate = crate::connection::ScriptGate::default();
        let _ = self.shared.script_gate.set(gate.clone());
        (self, gate)
    }

    /// Answer HostTimeStamp (op82) with an exception while the returned
    /// flag is set, as a host that fails the request would; the changes
    /// waiting for the client stay queued, since an exception carries none.
    /// A test fixture for clients that use op82 as a check, only in debug
    /// builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn failing_timestamps(self) -> (Self, Arc<AtomicBool>) {
        let flag = Arc::clone(&self.shared.failing_timestamps);
        (self, flag)
    }

    /// Close a connection with a session that receives nothing from its
    /// client for `timeout` after answering its last request (the time
    /// spent answering does not count), and one without a session after
    /// `timeout` too if that is shorter than its own limit: send a GIOP
    /// CloseConnection and end its session, as FileMaker Server closes
    /// silent connections (network-protocol.md 6.2; a client with a file
    /// open tickles every 60 s). [`ConnectionLimits::idle`] otherwise.
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.limits.idle = Some(timeout);
        self
    }

    /// Refuse client downloads under `path`: op90 and op92 fail with error
    /// 200, and op15 reads as missing. This test fixture lets tests see how
    /// a client handles a denied download. Separately, accounts without
    /// verified file Full Access cannot read account-member parents, other
    /// whole members, or `04` and `06` children. Their own whole member is
    /// an exception filtered to known metadata; query node reads follow
    /// that credential rule.
    pub fn refusing_downloads(self, path: &[&[u8]]) -> Self {
        self.shared
            .refused_downloads
            .write()
            .expect("refused downloads lock")
            .push(path.iter().map(|key| key.to_vec()).collect());
        self
    }

    /// Omit `path` from op92 copies without changing storage or op90.
    /// A test fixture for clients that must confirm incomplete batch reads.
    #[doc(hidden)]
    pub fn omitting_list_download(self, path: &[&[u8]]) -> Self {
        self.shared
            .omitted_list_paths
            .write()
            .expect("omitted list paths lock")
            .push(path.iter().map(|key| key.to_vec()).collect());
        self
    }

    /// Answer each client's Open of `file` only after `delay`, as a slow
    /// or busy host would. For tests of clients that must not keep waiting
    /// for files they no longer need.
    pub fn delaying_opens(self, file: &str, delay: Duration) -> Self {
        self.shared
            .open_delays
            .write()
            .expect("open delays lock")
            .insert(file.to_lowercase(), delay);
        self
    }

    /// Publish the hosted file clients open as `file` in their file list
    /// (the Hosts dialog's, OpenMaker's extension
    /// `om_protocol::hosted_files`). Files are unlisted until published:
    /// a file's own "Don't display in Open Remote File dialog" setting is
    /// not read yet, so no file is listed that its designer may have
    /// hidden. An unlisted file still opens by name, and a listed one
    /// still needs an account to open. The list is the same for every
    /// client: names only, never paths. A name the server does not host
    /// lists nothing.
    pub fn listing_file(self, file: &str) -> Self {
        if self.shared.hosting.by_name(file).is_none() {
            warn!("not listing {file:?}: it is not hosted");
        }
        self.shared
            .listed
            .write()
            .expect("listed files lock")
            .insert(file.to_lowercase());
        self
    }

    /// [`Server::listing_file`] for every hosted file.
    pub fn listing_all_files(self) -> Self {
        let names: Vec<String> = self
            .shared
            .hosting
            .files()
            .iter()
            .map(|file| file.name.to_lowercase())
            .collect();
        self.shared
            .listed
            .write()
            .expect("listed files lock")
            .extend(names);
        self
    }

    /// Answer every file list with `body` instead. A test fixture for
    /// clients that must reject a malformed or incomplete list.
    #[doc(hidden)]
    pub fn overriding_file_list(self, body: Vec<u8>) -> Self {
        *self
            .shared
            .file_list_override
            .write()
            .expect("file list override lock") = Some(body);
        self
    }

    /// Answer that the server has no OpenMaker file list (`_is_a` false),
    /// as a host without the extension does. A test fixture for clients
    /// that must report such a host as unsupported, not as listing nothing.
    #[doc(hidden)]
    pub fn without_file_list(self) -> Self {
        self.shared.no_file_list.store(true, Ordering::Relaxed);
        self
    }

    /// Answer GetFileList (op77) with error 3, as an operation the server
    /// does not support. With [`Server::without_file_list`], a test fixture
    /// for clients that must report a host offering neither list as
    /// unsupported.
    #[doc(hidden)]
    pub fn without_get_file_list(self) -> Self {
        self.shared.no_get_file_list.store(true, Ordering::Relaxed);
        self
    }

    /// Answer as a host without OpenMaker's authoring extension: `_is_a`
    /// false and opcode 240 unsupported. A test fixture for clients that
    /// must not send it such a host.
    #[doc(hidden)]
    pub fn without_authoring(self) -> Self {
        self.shared.no_authoring.store(true, Ordering::Relaxed);
        self
    }

    /// Replace the op90 subtree bytes at `path`, without changing storage.
    /// An opt-in test fixture for validating incomplete host replies.
    #[doc(hidden)]
    pub fn overriding_download_slots(self, path: &[&[u8]], slots: Vec<u8>) -> Self {
        self.shared
            .download_overrides
            .write()
            .expect("download overrides lock")
            .push(crate::connection::DownloadOverride {
                path: path.iter().map(|key| key.to_vec()).collect(),
                slots,
            });
        self
    }

    /// Answer every download and find (op90, op15, op92, op86) through a
    /// second, test-only view of the hosted files as well as the file
    /// engine's, and panic when the two answers differ
    /// (docs/server-storage.md 5, "Views"; `engine::copies`): a test
    /// fixture that shows readers do not lean on the file engine's view.
    /// Every operation's reads and writes through its transaction are
    /// checked too, against its starting tree with its writes applied in order
    /// ("The operations' transaction"; `engine::checking`), which shows
    /// each moved operation's writes say all it changed. Only in debug
    /// builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn checking_reads(self) -> Self {
        self.shared.engine.check_reads();
        self.shared.engine.check_writes();
        self
    }

    /// Answer a client's find on a field without the index it reads as
    /// FileMaker Server does: with no records, building the index
    /// afterwards for later finds (query-program.md 7.1). By default the
    /// server builds the index first and answers from it. For tests of
    /// clients, which must not take that first answer for "no match".
    /// Also models a false ExistsInIndex answer on a missing value index,
    /// so write-validation tests cannot be masked by eager index building.
    pub fn with_late_index_builds(self) -> Self {
        self.shared.late_index_builds.store(true, Ordering::Relaxed);
        self
    }

    /// Omit the closing slot of QueryProgram replies for tests of clients'
    /// incomplete-reply handling. Normal servers always send complete replies.
    #[doc(hidden)]
    pub fn with_unterminated_query_replies(self) -> Self {
        self.shared
            .unterminated_query_replies
            .store(true, Ordering::Relaxed);
        self
    }

    /// Write the event log in `folder`: FileMaker Server's `Event.log` and
    /// `Access.log` (`docs/re/server-logs.md`), each renamed to its `-old`
    /// file at [`crate::MAX_LOG_SIZE`]. The folder is made if need be
    /// (0700 on Unix) and refused when it is a link, or on Unix when it is
    /// not the server's user's or other users may write in it; the logs are
    /// reached through it, never through a link (`docs/admin-console.md`,
    /// "The event log and alerts"). What happened since [`Server::bind`]
    /// (the files it opened) is written first. Without it, the server keeps
    /// alerts but writes no log.
    pub fn with_event_log(self, folder: impl Into<PathBuf>) -> Result<Self, StartError> {
        let dir = folder.into();
        match self.shared.events.attach(&dir) {
            Ok(()) => {
                info!("writing the event log in {}", dir.display());
                Ok(self)
            }
            Err(error) => Err(StartError::EventLog { dir, error }),
        }
    }

    /// Run `schedules` (FileMaker script schedules) while the server
    /// serves; refused when one cannot run as given or two share a name.
    pub fn with_schedules(self, schedules: Vec<Schedule>) -> Result<Self, ScheduleError> {
        self.schedules.install(schedules, &self.shared)?;
        Ok(self)
    }

    /// The clock schedules follow (the server's own, [`crate::SystemClock`],
    /// otherwise).
    pub fn with_clock(self, clock: Arc<dyn Clock>) -> Self {
        self.schedules.set_clock(clock);
        self
    }

    /// The default backup folder: backup schedules without a target of
    /// their own put their backups there (FileMaker's Default Backup
    /// Folder). Without one, such a schedule's runs fail. It is made
    /// (owner-only) at the first backup if it is missing. Refused when it
    /// holds a hosted file or its container data, or lies in a hosted
    /// file's container data folder: old backups are deleted from it.
    pub fn with_backup_folder(self, folder: impl Into<PathBuf>) -> Result<Self, ScheduleError> {
        let folder = folder.into();
        crate::backups::check_folder(&self.shared, &folder).map_err(|reason| {
            ScheduleError::BackupFolder {
                folder: folder.clone(),
                reason,
            }
        })?;
        self.schedules.backups().set_folder(folder);
        Ok(self)
    }

    /// The database folders the server's files were looked for in at
    /// startup (`om_server::database_folders::discover`), which the Admin
    /// Console lists whether or not a hosted file lies in them, as
    /// FileMaker Server lists its default and additional database folders.
    pub fn with_database_folders(mut self, folders: Vec<PathBuf>) -> Self {
        self.database_folders = Arc::new(folders);
        self
    }

    /// A handle for administering the running server: its clients, its
    /// scripts, its schedules and its shutdown.
    pub fn handle(&self) -> io::Result<ServerHandle> {
        Ok(ServerHandle {
            shared: self.shared.clone(),
            schedules: self.schedules.clone(),
            addr: self.local_addr()?,
            database_folders: self.database_folders.clone(),
        })
    }

    /// The address the server listens on.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept connections, one thread per connection within the caps of
    /// its [`ConnectionLimits`], and run the schedules, until
    /// [`ServerHandle::shutdown`]. The cap on connections is lowered to
    /// what the open-file limit leaves room for, if that is less.
    pub fn serve(self) -> io::Result<()> {
        info!("listening on {}", self.local_addr()?);
        self.shared.events.serving();
        self.schedules.start(&self.shared);
        let mut limits = self.limits;
        let per_connection = DESCRIPTORS_PER_CONNECTION
            + self
                .shared
                .recordings
                .as_ref()
                .map_or(0, |_| DESCRIPTORS_PER_RECORDING);
        let files = self.shared.hosting.slots().len();
        if let Some(room) = descriptor_room(per_connection, files) {
            if room < limits.connections {
                warn!(
                    "the open-file limit leaves room for {room} connections, fewer than the \
                     {} allowed: serving at most {room} (raise the limit to serve more)",
                    limits.connections
                );
                limits.connections = room;
            }
        }
        let mut refusals = Throttled::default();
        let mut failed_accepts = Throttled::default();
        let mut failed_threads = Throttled::default();
        for stream in self.listener.incoming() {
            if self.shared.shutdown.load(Ordering::Acquire) {
                info!("shut down");
                break;
            }
            let stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    failed_accepts.warn(&format!("accept: {e}"));
                    if !matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::Interrupted
                    ) {
                        std::thread::sleep(ACCEPT_RETRY);
                    }
                    continue;
                }
            };
            let accepted = Instant::now();
            // A connection without a peer address has already gone.
            let Ok(address) = stream.peer_addr() else {
                continue;
            };
            // Over a cap, the connection is closed here, unread.
            let slot = match self.admissions.admit(address.ip(), &limits) {
                Ok(slot) => slot,
                Err(refused) => {
                    refusals.warn(&format!("{address}: connection refused: {refused}"));
                    continue;
                }
            };
            // Tracked from its accept to its end, so that it can be closed
            // during its handshake too (`Sockets`).
            let tracked = self.shared.sockets.track(&stream);
            let closed = tracked
                .as_ref()
                .map(|(_, closed)| closed.clone())
                .unwrap_or_default();
            let served = Served {
                socket: tracked.map(|(id, _)| id),
                shared: self.shared.clone(),
                _slot: slot,
            };
            let tls = self.tls.clone();
            let mut thread = std::thread::Builder::new();
            if let Some(bytes) = self.stack_size {
                thread = thread.stack_size(bytes);
            }
            let spawned = thread.spawn(move || {
                let served = served;
                // What the connection holds is released as the panic
                // unwinds (its session by `Connection`'s `Drop`, its slot
                // and socket by `Served`'s); it is only logged here.
                let shared = &served.shared;
                let limits = &limits;
                let served_to_end = panic::catch_unwind(AssertUnwindSafe(|| {
                    handle(stream, address, accepted, tls, shared, limits, closed)
                }));
                if let Err(panic) = served_to_end {
                    error!(
                        "{address}: connection ended by a panic in the server: {}",
                        panic_message(panic.as_ref())
                    );
                }
            });
            if let Err(e) = spawned {
                // The connection and its slot went with the closure: it is
                // closed, and the server goes on.
                failed_threads.warn(&format!(
                    "{address}: connection closed: no thread to serve it: {e}"
                ));
            }
        }
        Ok(())
    }
}

/// What a connection holds while it is served, released when its thread
/// ends however it ends: its place under the caps and its entry in
/// [`crate::connection::Sockets`], whose socket is shut down both ways so
/// that the client sees the connection end.
struct Served {
    shared: Arc<Shared>,
    socket: Option<u64>,
    _slot: Slot,
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Some(id) = self.socket {
            self.shared.sockets.close(id);
        }
    }
}

/// The most characters of a panic's message the log keeps.
const PANIC_MESSAGE_LIMIT: usize = 200;

/// A panic's message for the log: escaped, since it may quote what a
/// client sent, and cut to [`PANIC_MESSAGE_LIMIT`] characters.
fn panic_message(panic: &(dyn Any + Send)) -> String {
    let message = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str));
    let Some(message) = message else {
        return "(no message)".to_string();
    };
    let mut kept: String = message.chars().take(PANIC_MESSAGE_LIMIT).collect();
    if kept.len() < message.len() {
        kept.push_str("...");
    }
    format!("{kept:?}")
}

/// A warning of something that goes on while a condition lasts, logged at
/// most once a [`WARNING_INTERVAL`] with how many were left out, so that a
/// flood of connections does not flood the log too. The others go to the
/// debug log.
#[derive(Default)]
struct Throttled {
    warned: Option<Instant>,
    left_out: u64,
}

impl Throttled {
    fn warn(&mut self, message: &str) {
        if self
            .warned
            .is_some_and(|warned| warned.elapsed() < WARNING_INTERVAL)
        {
            self.left_out += 1;
            debug!("{message}");
            return;
        }
        if self.left_out > 0 {
            warn!(
                "{message} ({} more like it since the last warning)",
                self.left_out
            );
        } else {
            warn!("{message}");
        }
        self.warned = Some(Instant::now());
        self.left_out = 0;
    }
}

/// How many connections the open-file limit leaves room for, with
/// `per_connection` descriptors each, beside the descriptors open now and
/// [`DESCRIPTORS_FOR_WORK`] (and two per hosted file of `files`) for the
/// server's own work. `None` when no limit applies.
#[cfg(unix)]
fn descriptor_room(per_connection: usize, files: usize) -> Option<usize> {
    use rustix::process::{getrlimit, Resource};
    // `None` is unlimited.
    let limit = getrlimit(Resource::Nofile).current?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    // Counting what is open includes the listing's own descriptor.
    let open = std::fs::read_dir("/proc/self/fd")
        .or_else(|_| std::fs::read_dir("/dev/fd"))
        .map_or(0, Iterator::count);
    let work = DESCRIPTORS_FOR_WORK
        .saturating_add(files.saturating_mul(2))
        .min(limit / 4);
    Some(limit.saturating_sub(open.saturating_add(work)) / per_connection)
}

#[cfg(not(unix))]
fn descriptor_room(_per_connection: usize, _files: usize) -> Option<usize> {
    None
}

/// Administers a running server (from another thread than [`Server::serve`]'s):
/// the server, its files and its clients as the administrator sees them,
/// disconnecting a client (`crate::admin`), the Perform Script on Server
/// scripts executing, aborting them, the schedules, the backups, and the
/// server's shutdown.
#[derive(Clone)]
pub struct ServerHandle {
    pub(crate) shared: Arc<Shared>,
    schedules: Arc<Scheduler>,
    pub(crate) addr: SocketAddr,
    database_folders: Arc<Vec<PathBuf>>,
}

impl ServerHandle {
    /// This handle's reference to the server's state, for a caller that
    /// drops the state itself once no other thread holds it
    /// ([`crate::LocalHost::close`]).
    pub(crate) fn into_shared(self) -> Arc<Shared> {
        self.shared
    }

    /// The scripts executing now.
    pub fn scripts(&self) -> Vec<RunningScript> {
        self.shared.scripts.running()
    }

    /// Abort script `id` (an administrator's abort): it stops before its
    /// next step with its transaction and uncommitted record reverted and
    /// its locks released. Returns whether it was running.
    pub fn abort_script(&self, id: u64) -> bool {
        self.shared
            .scripts
            .abort(AbortReason::Admin, |script, _| script == id)
            > 0
    }

    /// Abort every script of the hosted file `file`, as closing the file
    /// does. Returns how many.
    pub fn abort_file_scripts(&self, file: &str) -> usize {
        let file = file.to_lowercase();
        self.shared
            .scripts
            .abort(AbortReason::FileClosed, |_, name| {
                name.to_lowercase() == file
            })
    }

    /// The schedules, with their next and last runs.
    pub fn schedules(&self) -> Vec<ScheduleStatus> {
        self.schedules.statuses()
    }

    /// Run schedule `name` now (also when it is disabled), on a thread of
    /// its own; [`ServerHandle::schedules`] shows its run.
    pub fn run_schedule(&self, name: &str) -> Result<(), ScheduleError> {
        self.schedules.run_now(&self.shared, name)
    }

    /// Enable or disable schedule `name`.
    pub fn enable_schedule(&self, name: &str, enabled: bool) -> Result<(), ScheduleError> {
        let name = self.schedules.set_enabled(name, enabled)?;
        self.shared
            .events
            .record(Event::schedule_enabled(&name, enabled));
        Ok(())
    }

    /// Wait up to `timeout` until no schedule's run executes and, with a
    /// clock that moves only when told to ([`crate::ManualClock`]), every
    /// run due at its time has started. Returns whether that happened.
    pub fn wait_for_schedules(&self, timeout: Duration) -> bool {
        self.schedules.wait_idle(timeout)
    }

    /// The database folders the server looked for files in at startup
    /// ([`Server::with_database_folders`]).
    pub fn database_folders(&self) -> Vec<PathBuf> {
        self.database_folders.to_vec()
    }

    /// The default backup folder ([`Server::with_backup_folder`]).
    pub fn backup_folder(&self) -> Option<PathBuf> {
        self.schedules.backups().folder()
    }

    /// Back up now with backup schedule `name` (also when it is disabled),
    /// on a thread of its own: an on-demand backup, labeled "On-demand",
    /// in the schedule's folder, kept by its keep count like the others.
    /// [`ServerHandle::schedules`] shows the run and
    /// [`ServerHandle::running_backups`] its progress. Refused for a
    /// schedule that is not a backup's, or whose last run has not ended.
    pub fn back_up_now(&self, name: &str) -> Result<(), ScheduleError> {
        self.schedules.back_up_now(&self.shared, name)
    }

    /// The backup runs under way, the one copying first; the others wait
    /// for it (one backup copies at a time). Empty: "No Backup Running".
    pub fn running_backups(&self) -> Vec<BackupProgress> {
        self.schedules.backups().progress()
    }

    /// Cancel the backup that is copying: it stops within a few megabytes,
    /// removes its unfinished folder and ends with error 1 (its schedule's
    /// last run says so). Returns whether one was copying.
    pub fn cancel_backup(&self) -> bool {
        self.schedules.backups().cancel()
    }

    /// The backups in the default backup folder and the backup schedules'
    /// folders, newest first: backups of schedules that no longer exist
    /// too, unfinished ones not. Each schedule's next backup is its
    /// [`ServerHandle::schedules`] entry's next run.
    pub fn backups(&self) -> Vec<BackupInfo> {
        self.schedules
            .backups()
            .list(&self.shared, &self.backup_folders())
    }

    /// Preserve backup `id` ([`BackupInfo::id`]), keeping it from
    /// automatic deletion, or stop preserving it (`preserved` false).
    pub fn preserve_backup(&self, id: &str, preserved: bool) -> Result<BackupInfo, BackupError> {
        self.schedules
            .backups()
            .set_preserved(&self.shared, &self.backup_folders(), id, preserved)
    }

    /// Give backup `id` the label `label`: one line of at most
    /// [`crate::MAX_LABEL_CHARS`] characters.
    pub fn set_backup_label(&self, id: &str, label: &str) -> Result<BackupInfo, BackupError> {
        self.schedules
            .backups()
            .set_label(&self.shared, &self.backup_folders(), id, label)
    }

    /// Delete backup `id`, which must be preserved (the others go by their
    /// schedule's keep count). Files it shares with other backups by hard
    /// links stay theirs.
    pub fn delete_backup(&self, id: &str) -> Result<(), BackupError> {
        self.schedules
            .backups()
            .delete(&self.shared, &self.backup_folders(), id)
    }

    /// Make the next backup's flush of its target folder, after the backup
    /// is renamed into place, fail. A test fixture, only in debug builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn failing_next_backup_flush(&self) {
        self.schedules.backups().fail_next_flush();
    }

    /// Make the backup folders act as on a file system without hard links:
    /// no copy is linked to an earlier backup's, and each is made under its
    /// name. A test fixture, only in debug builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn backing_up_without_hard_links(&self) {
        self.schedules.backups().refuse_hard_links();
    }

    /// Run `hook` each time a backup folder has been checked and is about
    /// to be removed (Delete, retention, an unfinished backup's cleanup). A
    /// test fixture, only in debug builds.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn before_backup_removal(&self, hook: impl FnMut() + Send + 'static) {
        self.schedules.backups().on_removal(Box::new(hook));
    }

    /// The default backup folder and the backup schedules' own, made
    /// absolute.
    fn backup_folders(&self) -> Vec<PathBuf> {
        let backups = self.schedules.backups();
        backups
            .folder()
            .into_iter()
            .chain(
                self.schedules
                    .backup_schedules()
                    .into_iter()
                    .filter_map(|(_, task)| task.folder.or_else(|| backups.folder())),
            )
            .map(|folder| std::path::absolute(&folder).unwrap_or(folder))
            .collect()
    }

    /// Close every accepted connection's socket at once, whether its client
    /// has a session, is in its TLS handshake or anything else: each
    /// connection ends as when its client leaves. Returns how many there
    /// were. For a host that is going away ([`crate::LocalHost`]); a
    /// server's [`ServerHandle::shutdown`] leaves open connections open.
    pub fn close_connections(&self) -> usize {
        self.shared.sockets.close_all()
    }

    /// Shut down: record it in the event log, stop the schedules (no run
    /// starts from now on), stop accepting connections (open connections
    /// are not closed), refuse new scripts (820), abort the executing ones
    /// (schedules' runs included), cancel the backups under way (each
    /// removes its unfinished folder), and wait up to `grace` for them to
    /// revert and release their locks. Returns whether every script and
    /// backup ended in time. The event log goes on recording what the open
    /// connections do; once they have ended,
    /// [`ServerHandle::close_event_log`] writes its last lines.
    pub fn shutdown(&self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        self.shared.events.record(Event::server_stopping());
        self.schedules.stop();
        self.schedules.backups().close();
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.scripts.close();
        self.shared
            .scripts
            .abort(AbortReason::Shutdown, |_, _| true);
        // Wake the accept loop so that it sees the flag.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        let scripts = self.shared.scripts.wait_idle(grace);
        let left = deadline.saturating_duration_since(Instant::now());
        self.schedules.backups().wait_idle(left) && scripts
    }

    /// End the event log of a server that has stopped
    /// ([`ServerHandle::shutdown`]) and whose connections have ended
    /// ([`ServerHandle::close_connections`]): wait up to `timeout` for the
    /// schedules' runs to record how they ended, record each hosted file
    /// closed and the server stopped, and write what is queued, waiting up
    /// to `timeout` again. Later events are not written. Returns whether
    /// the log was written to its end in time.
    pub fn close_event_log(&self, timeout: Duration) -> bool {
        self.schedules.wait_idle(timeout);
        let events = &self.shared.events;
        for file in self.shared.hosting.files() {
            events.record(Event::file_closed(&file.name));
        }
        events.record(Event::server_stopped());
        events.close(timeout)
    }
}

/// Serve the connection `tcp` from `address`, accepted at `accepted`;
/// `closed` is set when the server closes it
/// ([`crate::connection::Sockets::track`]).
fn handle(
    tcp: TcpStream,
    address: SocketAddr,
    accepted: Instant,
    tls: Arc<rustls::ServerConfig>,
    shared: &Arc<Shared>,
    limits: &ConnectionLimits,
    closed: Arc<AtomicBool>,
) {
    let peer = address.to_string();
    let _ = tcp.set_nodelay(true);
    let mut conn = match ServerConnection::new(tls) {
        Ok(conn) => conn,
        Err(e) => {
            warn!("{peer}: {e}");
            return;
        }
    };
    // Complete the handshake up front so a missing client certificate is
    // reported here rather than as a failed first read. A client that
    // stalls in it, or sends it a byte at a time, gives up its thread at
    // the deadline.
    let mut handshake = Deadline {
        tcp: &tcp,
        at: accepted + limits.handshake,
    };
    while conn.is_handshaking() {
        if let Err(e) = conn.complete_io(&mut handshake) {
            info!("{peer}: TLS handshake failed: {e}");
            return;
        }
    }
    let certificate = client_certificate(&conn);
    info!(
        "{peer}: connected, {}",
        tls_summary(&conn, certificate.as_ref())
    );
    // Wake up regularly to send back-channel calls (callback results) to a
    // quiet client; a read cut by the timeout resumes where it stopped. A
    // write fails once the client has taken nothing for the stall limit:
    // each byte it takes starts the wait again.
    for timeout in [
        tcp.set_read_timeout(Some(CALL_POLL)),
        tcp.set_write_timeout(Some(limits.write_stall)),
    ] {
        if let Err(e) = timeout {
            warn!("{peer}: {e}");
            return;
        }
    }
    let control = Arc::new(Control::default());
    let mut stream = StreamOwned::new(conn, ClientSocket::new(tcp, control.clone()));
    Connection::new(shared.clone(), address, control)
        .closed_by_server(closed)
        .with_client_certificate(certificate)
        .serve(&mut stream, limits);
    // TLS bytes still unsent mean the client stopped taking them: it is
    // not waited for again for a goodbye.
    if !stream.conn.wants_write() {
        stream.conn.send_close_notify();
        let _ = stream.conn.complete_io(&mut stream.sock);
    }
    info!("{peer}: disconnected");
}

/// The SHA-256 of the client certificate `conn`'s handshake verified
/// (`tls::AnyClientCert`): the one a joining transport must present again
/// (`crate::joins`).
fn client_certificate(conn: &ServerConnection) -> Option<[u8; 32]> {
    let der = conn.peer_certificates()?.first()?;
    let digest = ring::digest::digest(&ring::digest::SHA256, der.as_ref());
    digest.as_ref().try_into().ok()
}

/// What the log says of a connection's TLS: its version, whether its
/// handshake was full or resumed a session, and its client certificate's
/// SHA-256, which tells an administrator whether two connections came
/// from one client (`crate::joins`).
fn tls_summary(conn: &ServerConnection, certificate: Option<&[u8; 32]>) -> String {
    let version = conn
        .protocol_version()
        .map_or_else(|| "no TLS version".to_owned(), |v| format!("{v:?}"));
    let handshake = match conn.handshake_kind() {
        Some(rustls::HandshakeKind::Full) => "full handshake",
        Some(rustls::HandshakeKind::FullWithHelloRetryRequest) => {
            "full handshake after a HelloRetryRequest"
        }
        Some(rustls::HandshakeKind::Resumed) => "resumed session",
        None => "handshake of unknown kind",
    };
    let certificate = certificate.map_or_else(
        || "no client certificate".to_owned(),
        |hash| {
            let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
            format!("client certificate sha256 {hex}")
        },
    );
    format!("{version}, {handshake}, {certificate}")
}

/// A socket each read and write of which waits only for the time left
/// before `at`: TLS reads and writes the socket many times for one
/// handshake, and none of them goes past it however slowly the client
/// sends.
struct Deadline<'a> {
    tcp: &'a TcpStream,
    at: Instant,
}

impl Deadline<'_> {
    fn left(&self) -> io::Result<Duration> {
        let left = self.at.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the TLS handshake took too long",
            ));
        }
        Ok(left)
    }
}

impl Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.tcp.set_read_timeout(Some(self.left()?))?;
        let mut tcp = self.tcp;
        tcp.read(buf)
    }
}

impl Write for Deadline<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tcp.set_write_timeout(Some(self.left()?))?;
        let mut tcp = self.tcp;
        tcp.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
