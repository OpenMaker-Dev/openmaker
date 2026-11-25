//! OpenMaker Server: hosts `.fmp12` files for FileMaker-protocol clients.
//!
//! It speaks the RCFMP12 protocol over mutual TLS (GIOP 1.2), the way
//! FileMaker Server does on port 5003: `_is_a`, `Connect`, `Perform`, `Tickle`
//! and `Disconnect`. Inside `Perform` it handles Open (op78), Login (op80),
//! Download With Lock (op90), Download List (op92), QueryProgram (op86:
//! finds, relationship joins, sorts and found-set operations, via
//! `om-query`), the record write path (create op71, serials op72, upload
//! op14, commit op70, op103, locks op60/62/65) with change notifications to
//! the other clients, container data (store op88, reference counts op87,
//! thumbnails op96), Perform Script on Server (op30: scripts run by
//! `om-scripts` in a server session of their own with the account's
//! privileges, the file's record rules and OpenMaker's limits, callback
//! results sent as op105 on the back-channel; script schedules run the
//! same way on their own clock, [`Schedule`]), Manage Database's schema
//! editing (host clock
//! op82, unlocked downloads op15, lock upgrades op61, direct uploads op13,
//! the UploadMultiple and DeleteTable instructions of op86, deferred field
//! work op73 with the stored calculations a change makes stale recomputed,
//! and catalog change notices), Request Notification (op63) and
//! Logout (op81), paging large replies as FileMaker Server does; other
//! opcodes receive an "unavailable" status. Beside RCFMP12 it answers
//! OpenMaker's own file list (`om_protocol::hosted_files`) with the files
//! the operator publishes ([`Server::listing_file`]), and OpenMaker's own
//! script authoring extension (op240, `docs/authoring.md`), which replaces
//! a script's steps as one checked, saved catalog edit.
//!
//! Logins are checked against each file's own accounts, as FileMaker Server
//! checks them: the password against the hash the file stores, then the
//! account's kind, state and privilege set (`hosting::Eligibility`); the
//! server has no accounts of its own. A file FileMaker Server would not
//! host because anyone could open it with full access
//! ([`hosting::Insecure`]) is refused unless
//! [`ServerConfig::insecure_files`]. Logins are limited by how often they
//! fail from a client address and for an account name (`login_limits`): a
//! login past a limit gets FileMaker's error 214 without its password being
//! checked, and the server logs a warning when an address or account
//! reaches its limit.
//!
//! Each hosted file is loaded into memory as its key tree, which every
//! download, query and write uses; commits keep the record list and field
//! indexes current, and each change is written back as a complete new file
//! renamed over the old one (`om_hbam::writer`). Download replies are
//! byte-compatible with FileMaker Server's (verified against captured
//! traffic and the live server).
//!
//! A connection holds what its client uploads only for a request it may
//! run, and holds limited paged transfers: at most 8 transfers, 64 MiB in
//! those it is not using and one upload of 1 GiB (64 KiB for a file not
//! logged in to). An upload read into a tree is read in one pass, since a
//! list of short keys takes tens of times its size as a tree, and an op92
//! key list is never built into one. The trees of uploads being read and
//! used (records, commit lists, op13 subtrees, query programs, container
//! data) and the records clients stage are charged to one budget for the
//! whole server, 4 GiB by default (`ConnectionLimits::upload_memory`), of
//! which one client address may hold a quarter; what a request, a staged
//! area or a session held goes back however it ends. The downloads
//! connections keep paged share a budget of their own, 2 GiB by default
//! (`ConnectionLimits::download_memory`): past it the least recently paged
//! are dropped, on any connection. A request past a
//! limit fails with a CORBA system exception. These limits are OpenMaker's
//! choice, where a client that finishes each transfer and commits what it
//! stages never reaches them; FileMaker Server's are unmeasured.
//!
//! Each connection is served on a thread of its own, within caps on how
//! many are served at once, in all and from one client address; a TLS
//! handshake has a deadline, a silent connection is closed as FileMaker
//! Server closes one, and a client that stops taking replies is dropped
//! ([`ConnectionLimits`]).
//!
//! The server's TLS certificate is the administrator's, or else the
//! operating system's own or one the server makes once and keeps
//! ([`default_identity`]), so it stays the same across restarts.
//!
//! A [`LocalHost`] serves one local file privately to the client in the
//! same process that opened it: OpenMaker Client's File > Open, with the
//! file's own accounts and no license (`docs/re/local-files.md`).
//!
//! Backup schedules copy the hosted files on time or on demand, each run a
//! consistent copy of every chosen file in a folder laid out as FileMaker
//! Server lays one out, unchanged files hard-linked to the last backup,
//! old backups deleted past the schedule's keep count unless preserved
//! (`backups`, `docs/re/backups.md`).
//!
//! A [`ServerHandle`] administers the running server from another thread:
//! the server, its hosted files and its connected clients as the
//! administrator sees them ([`ServerOverview`], [`FileInfo`],
//! [`ConnectedClient`] with the files each has open and the account logged
//! in to each), disconnecting a client (at once, or after asking it to
//! close its files), sending it a message (a `Notify` call on its
//! connection), Block New Users, closing, opening, pausing and resuming a
//! hosted file, the Perform Script on Server scripts executing, the
//! schedules, the backups ([`BackupInfo`]: Back Up Now, preserve, label,
//! delete, cancel), the alerts and the event log's files ([`Alert`],
//! [`LogPage`]), and shutdown. OpenMaker's Admin Console (`om_admin`) is
//! built on it: [`console`] serves it beside the server, with the
//! `admin-console` feature (`docs/admin-console.md`).
//!
//! The server records what happens in an event log in FileMaker Server's
//! format, `Event.log` and `Access.log` in its log folder
//! ([`Server::with_event_log`], `docs/re/server-logs.md`).
//!
//! Spec: `docs/re/network-protocol.md`, `docs/re/write-path.md`,
//! `docs/re/containers.md`, `docs/re/schema-path.md`,
//! `docs/re/admin-console.md`, `docs/re/backups.md`. Written from the spec and captured traffic
//! only.

pub mod accounts;
mod admin;
mod admission;
mod anchored;
mod authoring;
mod backups;
mod cert;
pub mod config;
mod connection;
#[cfg(feature = "admin-console")]
pub mod console;
mod containers;
pub mod database_folders;
pub mod default_identity;
mod derived;
mod engine;
mod eventlog;
mod filelock;
pub mod hosting;
mod library;
mod local;
mod locks;
mod log_folder;
mod login_limits;
mod modcount;
mod recalc;
mod record;
#[cfg(feature = "sentry")]
pub mod reporting;
mod schedules;
mod scripts;
mod security;
mod server;
mod staged;
mod store;
mod thumbnail;
pub mod tls;
mod transfer;
mod uploads;
mod views;
mod writes;

pub use admin::{
    ClientFile, ConnectedClient, DisconnectError, FileError, FileInfo, FileStatus, MessageError,
    Notice, ServerOverview, DEFAULT_GRACE, MAX_GRACE, MAX_MESSAGE_CHARS, MAX_WAITING_MESSAGES,
};
pub use backups::{BackupError, BackupInfo, BackupProgress, MAX_KEEP, MAX_LABEL_CHARS};
pub use config::{
    ConnectionLimits, HostedFile, Privileges, ScriptLimits, ServerConfig, ServerIdentity,
    SCRIPT_CALC_LIMITS,
};
#[cfg(debug_assertions)]
#[doc(hidden)]
pub use connection::ScriptGate;
pub use connection::{
    LOGIN_INACTIVE_ACCOUNT, LOGIN_PASSWORD_CHANGE_DUE, LOGIN_UNREADABLE_ACCOUNT,
    LOGIN_UNREADABLE_FILE, LOGIN_UNSUPPORTED_PRIVILEGES,
};
pub use eventlog::{
    Alert, Level, LogEntry, LogFileInfo, LogPage, LogReadError, MAX_ALERTS, MAX_LOG_LINE,
    MAX_LOG_PAGE, MAX_LOG_SCAN, MAX_LOG_SIZE,
};
pub use local::{LocalHost, LocalHostError, ReadOnly};
pub use login_limits::LOGIN_TOO_MANY_ATTEMPTS;
pub use om_calc::eval::CalcLimits;
pub use schedules::{
    parse_schedules, BackupFiles, BackupTask, Clock, ClockWaker, Days, LastRun, ManualClock,
    Repeat, Schedule, ScheduleError, ScheduleFileError, ScheduleStatus, ScheduleTask, ScriptTask,
    SystemClock, Times,
};
pub use scripts::{AbortReason, RunningScript, DEFAULT_SCRIPT_LIMIT};
pub use server::{Server, ServerHandle, StartError};
#[cfg(debug_assertions)]
#[doc(hidden)]
pub use store::SaveFault;
pub use tls::Identity;
pub use writes::{MAX_LOCKS, MAX_QUEUED, MAX_QUEUED_BYTES};

/// Run `work` with the process's umask set to `mask`, then put the old one
/// back. The umask is the whole process's, so the tests that set it take
/// turns here.
#[cfg(all(test, unix))]
fn with_umask<T>(mask: rustix::fs::RawMode, work: impl FnOnce() -> T) -> T {
    use rustix::fs::Mode;
    use rustix::process::umask;
    struct Restore(Mode);
    impl Drop for Restore {
        fn drop(&mut self) {
            umask(self.0);
        }
    }
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _restore = Restore(umask(Mode::from_bits_truncate(mask)));
    work()
}
