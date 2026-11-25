//! OpenMaker Server's event log and alerts (`docs/admin-console.md`, "The
//! event log and alerts"; FileMaker Server's logs in
//! `docs/re/server-logs.md`).
//!
//! Events are written in FileMaker Server's format, one line each, to
//! `Event.log` (the server, its files, its schedules) and `Access.log`
//! (clients) in the log folder ([`crate::Server::with_event_log`]). Where
//! FileMaker Server has a message for an event, the line carries its number
//! and text; OpenMaker's own messages carry the number 0, which no
//! FileMaker message has. A log is renamed to `<name>-old.log` once it holds
//! [`MAX_LOG_SIZE`] bytes, replacing the previous one, as FileMaker Server
//! does.
//!
//! Events are formatted where they happen and written by a thread of the
//! log's own, through a bounded queue: recording one never waits for the
//! disk, and never fails what recorded it. When the queue is full the event
//! is counted and dropped, and the writer says how many it lost once it
//! catches up. A failed write is reported in the process log once, and
//! again only after writing worked in between.
//!
//! Alerts are the events the administrator must act on (a hosted file that
//! could not be opened or saved): kept in memory, newest first, at most
//! [`MAX_ALERTS`], until acknowledged ([`ServerHandle::alerts`]).
//!
//! The Log Viewer reads the files back ([`ServerHandle::read_log`]), newest
//! line first, a bounded page at a time, so it shows what is on disk. Each
//! read scans at most [`MAX_LOG_SCAN`] bytes; a longer line comes in parts.
//!
//! The log folder is opened once and checked, and the logs are reached
//! through it, never through a symbolic link ([`crate::log_folder`]).

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};

use log::{error, info};

use crate::hosting::HostingError;
use crate::log_folder::LogFolder;
use crate::scripts::AbortReason;
use crate::server::ServerHandle;

/// How large a log grows before it is renamed to `<name>-old.log` and a
/// new one started: 40 MiB, the size FileMaker Server's renamed logs were
/// (server-logs.md 1).
pub const MAX_LOG_SIZE: u64 = 40 << 20;

/// The most lines [`ServerHandle::read_log`] returns at once.
pub const MAX_LOG_PAGE: usize = 1000;

/// The most alerts kept; past it the oldest goes (OpenMaker's choice).
pub const MAX_ALERTS: usize = 100;

/// Lines waiting for the writer. Past it, events are counted and dropped.
const QUEUE: usize = 4096;

/// Lines kept before the server has a log folder or starts serving: the
/// files opened at startup, and room to spare.
const PENDING: usize = 1024;

/// The most characters of one value (a name, an error) a message holds.
const MAX_VALUE: usize = 256;

/// The most bytes of one line read back ([`LogEntry::cut`] past it). The
/// server's own lines are far shorter: each value in them is at most
/// [`MAX_VALUE`] characters.
pub const MAX_LOG_LINE: usize = 16 << 10;

/// The most bytes one [`ServerHandle::read_log`] reads of a file, so that
/// a page costs the same whatever the file holds.
pub const MAX_LOG_SCAN: u64 = 4 << 20;

/// How much of a log is read at a time, from its end.
const CHUNK: usize = 64 << 10;

/// The format of a line's timestamp (server-logs.md 2).
const TIMESTAMP: &str = "%Y-%m-%d %H:%M:%S%.3f %z";

/// An event's level, as a line spells it (server-logs.md 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Error,
    Warning,
    Information,
}

impl Level {
    /// The level as a line spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "Error",
            Level::Warning => "Warning",
            Level::Information => "Information",
        }
    }

    /// The level a line spells as `text`.
    pub fn parse(text: &str) -> Option<Self> {
        [Level::Error, Level::Warning, Level::Information]
            .into_iter()
            .find(|level| level.as_str() == text)
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The file an event goes to (server-logs.md 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Log {
    /// `Event.log`: the server, its files, its schedules.
    Event,
    /// `Access.log`: clients.
    Access,
}

impl Log {
    fn index(self) -> usize {
        self as usize
    }

    fn name(self) -> &'static str {
        match self {
            Log::Event => "Event.log",
            Log::Access => "Access.log",
        }
    }

    fn old_name(self) -> &'static str {
        match self {
            Log::Event => "Event-old.log",
            Log::Access => "Access-old.log",
        }
    }
}

/// The files of a log folder the server writes, and so reads back.
const LOG_FILES: [&str; 4] = ["Event.log", "Event-old.log", "Access.log", "Access-old.log"];

/// One event, as one line of a log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Event {
    log: Log,
    level: Level,
    /// FileMaker Server's number for the message (server-logs.md 4), or 0
    /// for OpenMaker's own.
    number: u16,
    message: String,
}

impl Event {
    fn new(log: Log, level: Level, number: u16, message: String) -> Self {
        Self {
            log,
            level,
            number,
            message,
        }
    }

    /// 486: the server accepts connections.
    pub fn server_started() -> Self {
        Self::new(
            Log::Event,
            Level::Information,
            486,
            "Database Server started.".into(),
        )
    }

    /// 488: the administrator stops the server.
    pub fn server_stopping() -> Self {
        Self::new(
            Log::Event,
            Level::Information,
            488,
            "Stopping Database Server...".into(),
        )
    }

    /// 490: the server no longer accepts connections.
    pub fn server_stopped() -> Self {
        Self::new(
            Log::Event,
            Level::Information,
            490,
            "Database Server stopped.".into(),
        )
    }

    /// 418: hosted file `name` is being opened.
    pub fn file_opening(name: &str) -> Self {
        Self::new(
            Log::Event,
            Level::Information,
            418,
            format!("Opening database \"{}\"...", database(name)),
        )
    }

    /// 184, or 18 for a file served read-only: hosted file `name` is open.
    pub fn file_opened(name: &str, read_only: bool) -> Self {
        let name = database(name);
        if read_only {
            Self::new(
                Log::Event,
                Level::Information,
                18,
                format!("Opened database \"{name}\" read only."),
            )
        } else {
            Self::new(
                Log::Event,
                Level::Information,
                184,
                format!("Opened database \"{name}\"."),
            )
        }
    }

    /// 168: hosted file `name` is closed.
    pub fn file_closed(name: &str) -> Self {
        Self::new(
            Log::Event,
            Level::Information,
            168,
            format!("Database \"{}\" closed.", database(name)),
        )
    }

    /// Hosted file `name` could not be opened: FileMaker Server's message
    /// for a missing file (250), one the server may not read (282), one
    /// that is not a FileMaker file (240) and one it does not host because
    /// anyone could open it with full access (1017, docs/re/server-logs.md
    /// 4), else OpenMaker's own with the error.
    pub fn file_open_failed(name: &str, error: &HostingError) -> Self {
        let name = database(name);
        let (number, message) = match error {
            HostingError::Open {
                error: om_hbam::Error::Io(e),
                ..
            } if e.kind() == io::ErrorKind::NotFound => (
                250,
                format!("Database \"{name}\" could not be opened; file was not found."),
            ),
            HostingError::Open {
                error: om_hbam::Error::Io(e),
                ..
            } if e.kind() == io::ErrorKind::PermissionDenied => (
                282,
                format!("Database \"{name}\" could not be opened: Permission denied."),
            ),
            HostingError::Open {
                error: om_hbam::Error::NotHbam,
                ..
            } => (
                240,
                format!("Database \"{name}\" could not be opened: not a FileMaker Pro database."),
            ),
            HostingError::Insecure { .. } => (
                1017,
                format!(
                    "SECURITY: Database \"{name}\" cannot be opened because it is insecure. \
                     Require Password-Protected Databases is enabled."
                ),
            ),
            other => (
                0,
                format!(
                    "Database \"{name}\" could not be opened: {}.",
                    value(&other.to_string())
                ),
            ),
        };
        Self::new(Log::Event, Level::Error, number, message)
    }

    /// Hosted file `name` is served read-only, for the reason `why`
    /// (something is at its save's copy name, its deferred work was left
    /// part way). OpenMaker's own message.
    pub fn hosted_read_only(name: &str, why: &str) -> Self {
        Self::new(
            Log::Event,
            Level::Warning,
            0,
            format!(
                "Database \"{}\" is served read-only: {}.",
                database(name),
                value(why)
            ),
        )
    }

    /// Saving hosted file `name` failed with `error`; `outcome` says what
    /// became of the changes. OpenMaker's own message: FileMaker Server
    /// writes its files a page at a time and has no such event.
    pub fn save_failed(name: &str, error: &dyn fmt::Display, outcome: SaveOutcome) -> Self {
        let outcome = match outcome {
            SaveOutcome::Kept => "Its unsaved changes are kept in memory.",
            SaveOutcome::TakenBack => "The change was taken back.",
            SaveOutcome::Unknown => {
                "The file may or may not keep the change, and takes no more changes until it \
                 is hosted again."
            }
        };
        Self::new(
            Log::Event,
            Level::Error,
            0,
            format!(
                "Saving database \"{}\" failed: {}. {outcome}",
                database(name),
                value(&error.to_string())
            ),
        )
    }

    /// 638: a client's first accepted Login: it is named by the session
    /// name it gave, its machine and address, and its version with the
    /// extended privilege it uses, `[fmapp]` (server-logs.md 3).
    pub fn client_connected(name: &str, machine: &str, address: IpAddr, version: &str) -> Self {
        Self::new(
            Log::Access,
            Level::Information,
            638,
            format!(
                "Client \"{}\" opening a connection from \"{} ({address})\" using \"{} [fmapp]\".",
                value(name),
                value(machine),
                value(version)
            ),
        )
    }

    /// 94: `account` logged in to hosted file `file`.
    pub fn client_opened(client: &ClientLabel, file: &str, account: &str) -> Self {
        Self::new(
            Log::Access,
            Level::Information,
            94,
            format!(
                "Client \"{client}\" opening database \"{}\" as \"{}\".",
                database(file),
                value(account)
            ),
        )
    }

    /// 98: `account` logged out of hosted file `file`, or its session
    /// ended with the file logged in to.
    pub fn client_closed(client: &ClientLabel, file: &str, account: &str) -> Self {
        Self::new(
            Log::Access,
            Level::Information,
            98,
            format!(
                "Client \"{client}\" closing database \"{}\" as \"{}\".",
                database(file),
                value(account)
            ),
        )
    }

    /// The end of a client's session: 22 when it left or was disconnected
    /// by the administrator, else OpenMaker's own message saying why.
    pub fn client_left(client: &ClientLabel, end: &SessionEnd) -> Self {
        let (level, number, message) = match end {
            SessionEnd::Closed => (
                Level::Information,
                22,
                format!("Client \"{client}\" closing a connection."),
            ),
            SessionEnd::Idle(limit) => (
                Level::Warning,
                0,
                format!(
                    "Client \"{client}\" disconnected: no request for {} seconds.",
                    limit.as_secs()
                ),
            ),
            SessionEnd::Lost(error) => (
                Level::Warning,
                0,
                format!(
                    "Client \"{client}\" disconnected: its connection failed: {}.",
                    value(error)
                ),
            ),
            SessionEnd::Overflow => (
                Level::Warning,
                0,
                format!(
                    "Client \"{client}\" disconnected: more change notifications waited for it \
                     than the server keeps."
                ),
            ),
            SessionEnd::ClosedByServer => (
                Level::Information,
                0,
                format!("Client \"{client}\" disconnected: the server closed its connection."),
            ),
            SessionEnd::Panic => (
                Level::Error,
                0,
                format!(
                    "Client \"{client}\" disconnected: the server failed answering its request."
                ),
            ),
        };
        Self::new(Log::Access, level, number, message)
    }

    /// The administrator disconnected the client
    /// ([`ServerHandle::disconnect_client`]). OpenMaker's own message: its
    /// disconnect sends the client no message, unlike FileMaker Server's
    /// 496 and 742.
    pub fn client_disconnected(client: &ClientLabel) -> Self {
        Self::new(
            Log::Access,
            Level::Warning,
            0,
            format!("Client \"{client}\" disconnected by the administrator."),
        )
    }

    /// 661: a Login to hosted file `file` naming `account` was refused.
    pub fn login_failed(client: &ClientLabel, file: &str, account: &str) -> Self {
        Self::new(
            Log::Access,
            Level::Warning,
            661,
            format!(
                "SECURITY: Client \"{client}\" authentication failed on database \"{}\" using \"{}\".",
                database(file),
                value(account)
            ),
        )
    }

    /// 1049: `account` failed to log in too often from every address; its
    /// logins are refused for a while (`login_limits`).
    pub fn account_locked(client: &ClientLabel, file: &str, account: &str) -> Self {
        Self::new(
            Log::Access,
            Level::Error,
            1049,
            format!(
                "SECURITY: Client \"{client}\" temporarily locked account \"{}\" on database \
                 \"{}\" because of too many failed attempts to log in.",
                value(account),
                database(file)
            ),
        )
    }

    /// Logins failed too often from the client's address, whose logins are
    /// refused for a while (`login_limits`). OpenMaker's own message.
    pub fn address_locked(client: &ClientLabel, file: &str) -> Self {
        Self::new(
            Log::Access,
            Level::Error,
            0,
            format!(
                "SECURITY: Client \"{client}\" temporarily locked its address out of database \
                 \"{}\" because of too many failed attempts to log in.",
                database(file)
            ),
        )
    }

    /// 148: a run of schedule `name` starts.
    pub fn schedule_running(name: &str) -> Self {
        Self::new(
            Log::Event,
            Level::Information,
            148,
            format!("Schedule \"{}\" running.", value(name)),
        )
    }

    /// 689: the run's script starts.
    pub fn schedule_script(name: &str, script: &str) -> Self {
        Self::new(
            Log::Event,
            Level::Information,
            689,
            format!(
                "Schedule \"{}\" has started FileMaker script \"{}\".",
                value(name),
                value(script)
            ),
        )
    }

    /// 150, or 644 with the script's last error: the run ended.
    pub fn schedule_completed(name: &str, error: i32) -> Self {
        let name = value(name);
        let message = if error == 0 {
            (150, format!("Schedule \"{name}\" completed."))
        } else {
            (
                644,
                format!("Schedule \"{name}\" completed; last scripting error ({error})."),
            )
        };
        Self::new(Log::Event, Level::Information, message.0, message.1)
    }

    /// 126: the schedule runs next at `next`, written as FileMaker Server
    /// writes it (`02/17/2026 10:35 AM`).
    pub fn schedule_next(name: &str, next: &jiff::Zoned) -> Self {
        let next = jiff::fmt::strtime::format("%m/%d/%Y %I:%M %p", next)
            .unwrap_or_else(|_| next.datetime().to_string());
        Self::new(
            Log::Event,
            Level::Information,
            126,
            format!("Schedule \"{}\" scheduled for {next}.", value(name)),
        )
    }

    /// 690: the run passed its time limit and goes on.
    pub fn schedule_over_time(name: &str) -> Self {
        Self::new(
            Log::Event,
            Level::Warning,
            690,
            format!("Schedule \"{}\" has exceeded its time limit.", value(name)),
        )
    }

    /// The run was aborted, or could not start, for `why`: FileMaker
    /// Server's message where it has one (152, 691, 732), else
    /// OpenMaker's own with the error.
    pub fn schedule_aborted(name: &str, why: ScheduleAbort<'_>) -> Self {
        let name = value(name);
        let (level, number, message) = match why {
            ScheduleAbort::Script(AbortReason::Admin, _) => (
                Level::Information,
                152,
                format!("Schedule \"{name}\" aborted; aborted by user."),
            ),
            ScheduleAbort::OwnTimeLimit | ScheduleAbort::Script(AbortReason::Shutdown, _) => (
                Level::Error,
                691,
                format!("Schedule \"{name}\" aborted; time limit exceeded or server stopped."),
            ),
            ScheduleAbort::Account => (
                Level::Error,
                732,
                format!("Schedule \"{name}\" aborted; invalid account name or password."),
            ),
            ScheduleAbort::Script(reason, error) => (
                Level::Error,
                0,
                format!(
                    "Schedule \"{name}\" aborted; {} (error {error}).",
                    abort_reason(reason)
                ),
            ),
            ScheduleAbort::NotHosted(file) => (
                Level::Error,
                0,
                format!(
                    "Schedule \"{name}\" aborted; database \"{}\" is not hosted.",
                    database(file)
                ),
            ),
            ScheduleAbort::Refused(why, error) => (
                Level::Error,
                0,
                format!("Schedule \"{name}\" aborted; {why} (error {error})."),
            ),
        };
        Self::new(Log::Event, level, number, message)
    }

    /// A run was due while the last one still ran. OpenMaker's own message:
    /// FileMaker Server's 1216 says to try again in 30 seconds, which
    /// OpenMaker does not.
    pub fn schedule_skipped(name: &str) -> Self {
        Self::new(
            Log::Event,
            Level::Warning,
            0,
            format!(
                "Schedule \"{}\" was skipped: its last run has not ended.",
                value(name)
            ),
        )
    }

    /// The administrator ran the schedule now
    /// ([`ServerHandle::run_schedule`]). OpenMaker's own message: FileMaker
    /// Server's 146 names the administrator, whom `ServerHandle` does not
    /// know.
    pub fn schedule_run_now(name: &str) -> Self {
        Self::new(
            Log::Event,
            Level::Information,
            0,
            format!("Schedule \"{}\" run now by the administrator.", value(name)),
        )
    }

    /// The administrator enabled or disabled the schedule
    /// ([`ServerHandle::enable_schedule`]; OpenMaker's own message, as
    /// for [`Event::schedule_run_now`]).
    pub fn schedule_enabled(name: &str, enabled: bool) -> Self {
        let state = if enabled { "enabled" } else { "disabled" };
        Self::new(
            Log::Event,
            Level::Information,
            0,
            format!("Schedule \"{}\" {state} by the administrator.", value(name)),
        )
    }

    /// A Perform Script on Server script was aborted. OpenMaker's own
    /// message: FileMaker Server's script engine logs script events in a
    /// form not read yet (server-logs.md 4).
    pub fn script_aborted(
        file: &str,
        script: &str,
        account: &str,
        reason: AbortReason,
        error: u16,
    ) -> Self {
        Self::new(
            Log::Event,
            Level::Warning,
            0,
            format!(
                "Perform Script on Server script \"{}\" in database \"{}\" for account \"{}\" \
                 aborted; {} (error {error}).",
                value(script),
                database(file),
                value(account),
                abort_reason(reason)
            ),
        )
    }

    /// 226 and 228: the full log was renamed, ending it and starting the new
    /// one.
    fn rotated(log: Log) -> (Self, Self) {
        (
            Self::new(
                log,
                Level::Information,
                226,
                "This log file reached maximum size, and was renamed. A new log file was created."
                    .into(),
            ),
            // The trailing space is FileMaker Server's.
            Self::new(
                log,
                Level::Information,
                228,
                format!(
                    "The previous log file reached maximum size, and was renamed to \"{}\". ",
                    log.old_name()
                ),
            ),
        )
    }

    /// `count` events were dropped: they came faster than the writer
    /// wrote. OpenMaker's own message.
    fn lost(count: u64) -> Self {
        Self::new(
            Log::Event,
            Level::Warning,
            0,
            format!(
                "{count} events were not logged: they came faster than the log could be written."
            ),
        )
    }

    /// The event as a line of a log, stamped `time`, from `host`.
    fn line(&self, time: &jiff::Zoned, host: &str) -> String {
        let timestamp = jiff::fmt::strtime::format(TIMESTAMP, time)
            .unwrap_or_else(|_| time.timestamp().to_string());
        format!(
            "{timestamp}\t{}\t{}\t{host}\t{}\n",
            self.level, self.number, self.message
        )
    }
}

/// What became of a hosted file's changes when saving it failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveOutcome {
    /// They stay in memory for the next save.
    Kept,
    /// The change that made them was taken back.
    TakenBack,
    /// The new copy was renamed over the file, but the folder could not be
    /// flushed: the file keeps the change in memory and takes no more.
    Unknown,
}

/// How a client's session ended, for its last line in `Access.log`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum SessionEnd {
    /// The client left (Disconnect, its closing the connection, a new
    /// Connect), or the administrator disconnected it.
    #[default]
    Closed,
    /// It sent nothing for its idle limit.
    Idle(Duration),
    /// Reading or writing its connection failed.
    Lost(String),
    /// More change notifications waited for it than the server keeps.
    Overflow,
    /// The server closed its connection
    /// ([`ServerHandle::close_connections`], as `om-server` does when it
    /// stops).
    ClosedByServer,
    /// A request handler panicked.
    Panic,
}

/// Why a schedule's run was aborted or could not start.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ScheduleAbort<'a> {
    /// Its script was aborted, with this error.
    Script(AbortReason, i32),
    /// It reached the schedule's own time limit, which ends it.
    OwnTimeLimit,
    /// Its account cannot log in to its file.
    Account,
    /// Its file is not hosted.
    NotHosted(&'a str),
    /// The server would not run it: why, and the error.
    Refused(&'a str, i32),
}

/// Why a script was aborted, as a message says it.
fn abort_reason(reason: AbortReason) -> &'static str {
    match reason {
        AbortReason::Admin => "an administrator aborted it",
        AbortReason::FileClosed => "its database was closed",
        AbortReason::Shutdown => "the server is stopping",
        AbortReason::StepLimit => "it reached the server's step limit",
        AbortReason::TimeLimit => "it reached the server's time limit",
    }
}

/// How `Access.log` names a client once it has logged in:
/// `<session name> (<machine name>) [<address>]` (server-logs.md 3), its
/// values escaped as [`value`] escapes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClientLabel(String);

impl ClientLabel {
    pub fn new(name: &str, machine: &str, address: IpAddr) -> Self {
        Self(format!("{} ({}) [{address}]", value(name), value(machine)))
    }
}

impl fmt::Display for ClientLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `text` as a message holds a value: at most [`MAX_VALUE`] characters,
/// with control characters (tabs and line breaks included) escaped, so that
/// what a client or a file names cannot add a field or a line.
pub(crate) fn value(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_VALUE));
    for (count, c) in text.chars().enumerate() {
        if count == MAX_VALUE {
            out.push_str("...");
            break;
        }
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// A hosted file's name as FileMaker Server's lines give a database's:
/// without `.fmp12` (server-logs.md 3).
pub(crate) fn database(name: &str) -> String {
    let len = name.len();
    let stem = match name.get(len.saturating_sub(6)..) {
        Some(extension) if len > 6 && extension.eq_ignore_ascii_case(".fmp12") => &name[..len - 6],
        _ => name,
    };
    value(stem)
}

/// An alert: something the administrator must act on, shown under the
/// console's bell until acknowledged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    /// For [`ServerHandle::acknowledge_alert`].
    pub id: u64,
    /// When it last happened.
    pub time: SystemTime,
    pub level: Level,
    /// The event's message, as the event log has it.
    pub message: String,
}

/// A log file in the log folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFileInfo {
    /// Its name, for [`ServerHandle::read_log`]: `Event.log`, `Access.log`
    /// or one of their `-old` files.
    pub name: String,
    /// Its size in bytes.
    pub size: u64,
    /// When it was last written, if the file system says.
    pub modified: Option<SystemTime>,
}

/// One line of a log, as it is on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    /// Where the line (or the part of it here) starts in the file:
    /// [`LogPage::next`] of a page that ends with it.
    pub offset: u64,
    /// Only part of the line is here: its first [`MAX_LOG_LINE`] bytes, or,
    /// when the page ended inside a longer line, the part read; the rest
    /// comes in the next page as entries of their own.
    pub cut: bool,
    /// The timestamp as written.
    pub timestamp: String,
    /// The timestamp, when it reads as one.
    pub time: Option<SystemTime>,
    /// The level as written ([`Level::parse`]).
    pub level: String,
    /// The message's number as written: FileMaker Server's, or 0 for
    /// OpenMaker's own messages.
    pub number: String,
    pub host: String,
    /// The message. A line that is not five tab-separated fields is all
    /// here, the other fields empty.
    pub message: String,
}

/// A page of a log's lines, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogPage {
    pub entries: Vec<LogEntry>,
    /// Where the next page, of older lines, ends: give it as `before` to
    /// [`ServerHandle::read_log`]; `None` when this page reaches the start
    /// of the file. It is inside a line when this page ended inside it
    /// (its last entry is [`LogEntry::cut`]); the next page then starts
    /// with the rest of that line, cut too.
    pub next: Option<u64>,
}

/// Why a log cannot be read.
#[derive(Debug)]
pub enum LogReadError {
    /// The server writes no event log.
    NoLog,
    /// The name is not one of the log files ([`ServerHandle::log_files`]).
    UnknownFile,
    Io(io::Error),
}

impl fmt::Display for LogReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogReadError::NoLog => f.write_str("the server writes no event log"),
            LogReadError::UnknownFile => f.write_str("no such log file"),
            LogReadError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LogReadError {}

/// The server's event log and alerts. A clone shares them. The default
/// records nothing: a client's private host ([`crate::LocalHost`]).
#[derive(Clone, Default)]
pub(crate) struct EventLog(Option<Arc<Inner>>);

struct Inner {
    /// The host field of every line.
    host: String,
    output: Mutex<Output>,
    /// Lines dropped because the queue was full, told once the writer
    /// catches up.
    dropped: Arc<AtomicU64>,
    alerts: Mutex<Alerts>,
    folder: OnceLock<Arc<LogFolder>>,
}

/// Where recorded lines go.
enum Output {
    /// Kept until the log folder is known ([`EventLog::attach`]), at most
    /// [`PENDING`].
    Pending(Vec<(Log, String)>),
    /// Queued for the writer, which says it is done on `done`.
    Writing {
        queue: SyncSender<(Log, String)>,
        done: Receiver<()>,
    },
    /// Nowhere: the server has no log folder, or stopped.
    Off,
}

#[derive(Default)]
struct Alerts {
    /// Newest first.
    list: Vec<Alert>,
    next_id: u64,
}

impl EventLog {
    /// A server's event log, whose lines name `host`: lines wait until
    /// [`EventLog::attach`] gives it a folder; alerts are kept from the
    /// start.
    pub fn new(host: String) -> Self {
        Self(Some(Arc::new(Inner {
            host: value(&host),
            output: Mutex::new(Output::Pending(Vec::new())),
            dropped: Arc::default(),
            alerts: Mutex::default(),
            folder: OnceLock::new(),
        })))
    }

    /// Record `event`.
    pub fn record(&self, event: Event) {
        let Some(inner) = &self.0 else {
            return;
        };
        let line = event.line(&jiff::Zoned::now(), &inner.host);
        let mut output = inner.output();
        match &mut *output {
            Output::Pending(lines) if lines.len() < PENDING => lines.push((event.log, line)),
            Output::Pending(_) => {
                inner.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Output::Writing { queue, .. } => match queue.try_send((event.log, line)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    inner.dropped.fetch_add(1, Ordering::Relaxed);
                }
                // The writer is gone; it said why in the process log.
                Err(TrySendError::Disconnected(_)) => {}
            },
            Output::Off => {}
        }
    }

    /// Record `event` and raise it as an alert. An alert with the same
    /// message not acknowledged yet is replaced, so that a condition that
    /// goes on is one alert, at the top.
    pub fn alert(&self, event: Event) {
        let Some(inner) = &self.0 else {
            return;
        };
        let mut alerts = inner.alerts();
        alerts.list.retain(|alert| alert.message != event.message);
        alerts.list.truncate(MAX_ALERTS - 1);
        alerts.next_id += 1;
        let alert = Alert {
            id: alerts.next_id,
            time: SystemTime::now(),
            level: event.level,
            message: event.message.clone(),
        };
        alerts.list.insert(0, alert);
        drop(alerts);
        self.record(event);
    }

    /// Write the log in `folder` from now on, starting with the lines kept
    /// so far. The folder is made if need be and checked
    /// ([`LogFolder::open`]), and both logs are opened now, so that a folder
    /// the server cannot use stops it at its start.
    pub fn attach(&self, folder: &Path) -> io::Result<()> {
        let Some(inner) = &self.0 else {
            return Ok(());
        };
        let folder = Arc::new(LogFolder::open(folder)?);
        for log in [Log::Event, Log::Access] {
            folder.secure(log.old_name())?;
        }
        let mut writer = Writer::new(folder.clone(), inner.host.clone(), MAX_LOG_SIZE)?;
        writer.dropped = inner.dropped.clone();
        let mut output = inner.output();
        let Output::Pending(pending) = &mut *output else {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the event log is already being written",
            ));
        };
        let pending = std::mem::take(pending);
        let (queue, lines) = mpsc::sync_channel(QUEUE);
        let (finished, done) = mpsc::channel();
        std::thread::Builder::new()
            .name("om-server event log".into())
            .spawn(move || {
                writer.run(&lines);
                let _ = finished.send(());
            })?;
        for line in pending {
            if queue.try_send(line).is_err() {
                inner.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        *output = Output::Writing { queue, done };
        let _ = inner.folder.set(folder);
        Ok(())
    }

    /// The server starts serving: record it, and stop keeping lines if it
    /// has no log folder.
    pub fn serving(&self) {
        self.record(Event::server_started());
        if let Some(inner) = &self.0 {
            let mut output = inner.output();
            if matches!(*output, Output::Pending(_)) {
                *output = Output::Off;
            }
        }
    }

    /// Stop writing: what is queued is written, waiting up to `timeout` for
    /// it; later events are not. Returns whether the writer finished in
    /// time.
    pub fn close(&self, timeout: Duration) -> bool {
        let Some(inner) = &self.0 else {
            return true;
        };
        let output = std::mem::replace(&mut *inner.output(), Output::Off);
        match output {
            Output::Writing { queue, done } => {
                drop(queue);
                done.recv_timeout(timeout).is_ok()
            }
            Output::Pending(_) | Output::Off => true,
        }
    }

    /// The folder the log is written in, once there is one.
    pub fn folder(&self) -> Option<&LogFolder> {
        self.0.as_ref()?.folder.get().map(Arc::as_ref)
    }

    /// The alerts, newest first.
    pub fn alerts(&self) -> Vec<Alert> {
        self.0
            .as_ref()
            .map(|inner| inner.alerts().list.clone())
            .unwrap_or_default()
    }

    /// Acknowledge alert `id`: it is no longer listed. Returns whether it
    /// was.
    pub fn acknowledge(&self, id: u64) -> bool {
        let Some(inner) = &self.0 else {
            return false;
        };
        let mut alerts = inner.alerts();
        let before = alerts.list.len();
        alerts.list.retain(|alert| alert.id != id);
        alerts.list.len() < before
    }
}

impl Inner {
    /// The output, also after a thread panicked holding it (each change
    /// leaves it whole).
    fn output(&self) -> MutexGuard<'_, Output> {
        self.output.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn alerts(&self) -> MutexGuard<'_, Alerts> {
        self.alerts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The log's writer: appends lines to the two logs and renames a full one.
struct Writer {
    folder: Arc<LogFolder>,
    host: String,
    max_size: u64,
    /// `Event.log` and `Access.log` ([`Log::index`]), each opened when
    /// first written, with its size.
    files: [(Option<File>, u64); 2],
    /// Shared with [`Inner::dropped`].
    dropped: Arc<AtomicU64>,
    /// For each log: a write failed and was reported; the next failure is
    /// not, until a write to it works again.
    failing: [bool; 2],
}

impl Writer {
    /// A writer of the logs in `folder`, both opened now.
    fn new(folder: Arc<LogFolder>, host: String, max_size: u64) -> io::Result<Self> {
        let event = folder.append(Log::Event.name())?;
        let access = folder.append(Log::Access.name())?;
        let (event_size, access_size) = (event.metadata()?.len(), access.metadata()?.len());
        Ok(Self {
            folder,
            host,
            max_size,
            files: [(Some(event), event_size), (Some(access), access_size)],
            dropped: Arc::default(),
            failing: [false; 2],
        })
    }

    /// Write lines until the queue is closed and empty.
    fn run(&mut self, lines: &Receiver<(Log, String)>) {
        while let Ok((log, line)) = lines.recv() {
            self.write(log, &line);
            while let Ok((log, line)) = lines.try_recv() {
                self.write(log, &line);
            }
            // Caught up: say how many there was no room for.
            let lost = self.dropped.swap(0, Ordering::Relaxed);
            if lost > 0 {
                let event = Event::lost(lost);
                self.write(event.log, &event.line(&jiff::Zoned::now(), &self.host));
            }
        }
    }

    /// Write one line, reporting a failure in the process log once.
    fn write(&mut self, log: Log, line: &str) {
        let written = self.append(log, line);
        let failing = &mut self.failing[log.index()];
        match written {
            Ok(()) if *failing => {
                *failing = false;
                info!(
                    "event log: writing to {} again",
                    self.folder.path().join(log.name()).display()
                );
            }
            Ok(()) => {}
            Err(e) if !*failing => {
                *failing = true;
                error!(
                    "event log: cannot write to {}: {e}; events are not logged until writing \
                     works again",
                    self.folder.path().join(log.name()).display()
                );
            }
            Err(_) => {}
        }
    }

    /// Append `line` to `log`, renaming it first if it is full. A log that
    /// cannot be renamed takes no more lines, so that it stays bounded.
    fn append(&mut self, log: Log, line: &str) -> io::Result<()> {
        let (file, size) = &mut self.files[log.index()];
        if file.is_none() {
            let opened = self.folder.append(log.name())?;
            *size = opened.metadata()?.len();
            *file = Some(opened);
        }
        if *size >= self.max_size {
            self.rotate(log)?;
        }
        let (file, size) = &mut self.files[log.index()];
        let written = file
            .as_mut()
            .map_or(Ok(()), |file| file.write_all(line.as_bytes()));
        match written {
            Ok(()) => {
                *size += line.len() as u64;
                Ok(())
            }
            Err(e) => {
                // Opened again for the next line: the folder may have gone.
                *file = None;
                Err(e)
            }
        }
    }

    /// Rename the full `log` to its `-old` name, replacing the previous
    /// one, and start a new one, each saying why (226, 228).
    fn rotate(&mut self, log: Log) -> io::Result<()> {
        self.files[log.index()].0 = None;
        self.folder.rename(log.name(), log.old_name())?;
        let time = jiff::Zoned::now();
        let (ended, started) = Event::rotated(log);
        // The renamed file's last line; it is complete without it.
        let _ = self
            .folder
            .append(log.old_name())
            .and_then(|mut file| file.write_all(ended.line(&time, &self.host).as_bytes()));
        let mut file = self.folder.append(log.name())?;
        let first = started.line(&time, &self.host);
        file.write_all(first.as_bytes())?;
        self.files[log.index()] = (Some(file), first.len() as u64);
        Ok(())
    }
}

/// Read a page of the log `file`, `len` bytes long: up to `limit` lines
/// (at most [`MAX_LOG_PAGE`]) ending before `before` (the end of the file
/// when `None`), newest first, reading at most `budget` bytes. Empty lines
/// are skipped.
///
/// Whether `before` falls inside a line is in the file: a page ends at a
/// line's start, after a line feed, or inside a line it cut. So from a
/// page's `next`, what precedes it back to the line feed before is the
/// start of a line the last page cut: it comes first, cut. From the end of
/// the file, a last line still being written (not ended by a line feed) is
/// left for a later page when its start is within the budget, and else
/// comes in cut parts like any line longer than the budget. When the
/// budget runs out inside a line, what was read of it is the page's last
/// entry, cut, and `next` is where it stopped.
fn read_page(
    file: &mut (impl Read + Seek),
    len: u64,
    before: Option<u64>,
    limit: usize,
    budget: u64,
) -> io::Result<LogPage> {
    let limit = limit.clamp(1, MAX_LOG_PAGE);
    let end = before.map_or(len, |before| before.min(len));
    // One byte of the budget is kept for the byte before where it stops.
    let floor = end.saturating_sub(budget.saturating_sub(1).max(1));
    let mut entries = Vec::new();
    // Collecting the segment that ends at `end`: from the end of the file
    // a line still being written, from a page's `next` a cut line's start.
    let mut first = true;
    // The segment being collected, from the back: its first
    // [`MAX_LOG_LINE`] bytes, and its whole length.
    let mut line: Vec<u8> = Vec::new();
    let mut line_len: u64 = 0;
    let mut pos = end;
    let mut chunk = vec![0; CHUNK];
    while pos > floor {
        let start = pos.saturating_sub(CHUNK as u64).max(floor);
        let read = (pos - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk[..read])?;
        let mut cut = read;
        while let Some(feed) = chunk[..cut].iter().rposition(|&byte| byte == b'\n') {
            let offset = start + feed as u64 + 1;
            prepend(&mut line, &chunk[feed + 1..cut]);
            line_len += (cut - feed - 1) as u64;
            // The line being written at the end of the file waits.
            let shown = !first || before.is_some();
            if shown && line_len > 0 {
                let cut = first || line_len > MAX_LOG_LINE as u64;
                entries.push(parse_entry(offset, &line, cut));
                if entries.len() == limit {
                    return Ok(LogPage {
                        entries,
                        next: Some(offset),
                    });
                }
            }
            first = false;
            line.clear();
            line_len = 0;
            cut = feed;
        }
        prepend(&mut line, &chunk[..cut]);
        line_len += cut as u64;
        pos = start;
    }
    if pos == 0 {
        // The file's first line; or, with no line feed at all, a line still
        // being written, or the start of the line a cursor is inside.
        let shown = !first || before.is_some();
        if shown && line_len > 0 {
            let cut = first || line_len > MAX_LOG_LINE as u64;
            entries.push(parse_entry(0, &line, cut));
        }
        return Ok(LogPage {
            entries,
            next: None,
        });
    }
    // The budget ran out at `pos`: what was collected is a whole line when
    // a line feed is just before it, else part of one.
    let mut previous = [0];
    file.seek(SeekFrom::Start(pos - 1))?;
    file.read_exact(&mut previous)?;
    let whole = previous[0] == b'\n';
    let waiting = first && before.is_none() && whole;
    if line_len > 0 && !waiting {
        let cut = first || !whole || line_len > MAX_LOG_LINE as u64;
        entries.push(parse_entry(pos, &line, cut));
    }
    Ok(LogPage {
        entries,
        next: Some(pos),
    })
}

/// Put `bytes`, read before `line` in the file, in front of it, keeping the
/// line's first [`MAX_LOG_LINE`] bytes.
fn prepend(line: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let mut joined = Vec::with_capacity((bytes.len() + line.len()).min(MAX_LOG_LINE));
    joined.extend_from_slice(&bytes[..bytes.len().min(MAX_LOG_LINE)]);
    let room = MAX_LOG_LINE - joined.len();
    joined.extend_from_slice(&line[..line.len().min(room)]);
    *line = joined;
}

/// A line of a log (or the part of it in `bytes`, when `cut`), starting at
/// `offset`, without its line feed.
fn parse_entry(offset: u64, bytes: &[u8], cut: bool) -> LogEntry {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_suffix('\r').unwrap_or(&text);
    let mut fields = text.splitn(5, '\t');
    match [(); 5].map(|()| fields.next()) {
        [Some(timestamp), Some(level), Some(number), Some(host), Some(message)] => LogEntry {
            offset,
            cut,
            timestamp: timestamp.into(),
            time: jiff::fmt::strtime::parse(TIMESTAMP, timestamp)
                .and_then(|time| time.to_timestamp())
                .ok()
                .map(SystemTime::from),
            level: level.into(),
            number: number.into(),
            host: host.into(),
            message: message.into(),
        },
        _ => LogEntry {
            offset,
            cut,
            timestamp: String::new(),
            time: None,
            level: String::new(),
            number: String::new(),
            host: String::new(),
            message: text.into(),
        },
    }
}

/// The machine's host name for the log's host field, its first label as
/// FileMaker Server's lines give it (server-logs.md 2), or `fallback`.
pub(crate) fn host_name(fallback: &str) -> String {
    #[cfg(unix)]
    let name = rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned();
    #[cfg(not(unix))]
    let name = std::env::var("COMPUTERNAME").unwrap_or_default();
    let first = name.split('.').next().unwrap_or_default().trim();
    if first.is_empty() {
        fallback.to_owned()
    } else {
        first.to_owned()
    }
}

impl ServerHandle {
    /// The alerts the administrator has not acknowledged, newest first: a
    /// hosted file that could not be opened at startup or saved. At most
    /// [`MAX_ALERTS`]; each is also in the event log.
    pub fn alerts(&self) -> Vec<Alert> {
        self.shared.events.alerts()
    }

    /// Acknowledge alert `id` ([`Alert::id`]): it is no longer listed.
    /// Returns whether it was.
    pub fn acknowledge_alert(&self, id: u64) -> bool {
        self.shared.events.acknowledge(id)
    }

    /// The folder the event log is written in, if the server writes one
    /// ([`crate::Server::with_event_log`]).
    pub fn log_folder(&self) -> Option<PathBuf> {
        self.shared
            .events
            .folder()
            .map(|folder| folder.path().to_path_buf())
    }

    /// The log files in the log folder: `Event.log`, `Access.log` and their
    /// `-old` files, those that exist. Empty without an event log.
    pub fn log_files(&self) -> Vec<LogFileInfo> {
        let Some(folder) = self.shared.events.folder() else {
            return Vec::new();
        };
        LOG_FILES
            .iter()
            .filter_map(|name| {
                let (size, modified) = folder.stat(name)?;
                Some(LogFileInfo {
                    name: (*name).to_owned(),
                    size,
                    modified,
                })
            })
            .collect()
    }

    /// A page of the log file `name` ([`LogFileInfo::name`]), newest line
    /// first: up to `limit` lines (at most [`MAX_LOG_PAGE`]) before the
    /// offset `before`, or from the end of the file when it is `None`. The
    /// lines are read from the file, so this is what is on disk. A page's
    /// [`LogPage::next`] gives the next one; after the log is renamed
    /// (rotated) the offsets of the old pages no longer apply. A page reads
    /// at most [`MAX_LOG_SCAN`] bytes, so it may hold fewer lines than
    /// `limit` and still have a `next`; a line longer than
    /// [`MAX_LOG_LINE`], or one the page ended inside, is [`LogEntry::cut`].
    /// The file is opened through the checked folder, never through a
    /// link.
    pub fn read_log(
        &self,
        name: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<LogPage, LogReadError> {
        let folder = self.shared.events.folder().ok_or(LogReadError::NoLog)?;
        // Only the server's own files, by their exact names: never a path.
        let name = LOG_FILES
            .iter()
            .find(|known| **known == name)
            .ok_or(LogReadError::UnknownFile)?;
        let mut file = folder.read(name).map_err(LogReadError::Io)?;
        let len = file.metadata().map_err(LogReadError::Io)?.len();
        read_page(&mut file, len, before, limit, MAX_LOG_SCAN).map_err(LogReadError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer for `folder` renaming a log at `max_size` bytes.
    fn writer(folder: &Path, max_size: u64) -> Writer {
        let folder = Arc::new(LogFolder::open(folder).unwrap());
        Writer::new(folder, "server".into(), max_size).unwrap()
    }

    fn read_all(path: &Path) -> Vec<LogEntry> {
        let mut file = File::open(path).unwrap();
        let len = file.metadata().unwrap().len();
        let mut entries = Vec::new();
        let mut before = None;
        loop {
            let page = read_page(&mut file, len, before, 2, MAX_LOG_SCAN).unwrap();
            entries.extend(page.entries);
            match page.next {
                Some(next) => before = Some(next),
                None => return entries,
            }
        }
    }

    #[test]
    fn lines_round_trip_through_rotation_and_paging() {
        let folder = std::env::temp_dir().join(format!("om-eventlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);

        // A line FileMaker Server wrote (server-logs.md 2, stand-in names),
        // as the reader takes it.
        let observed = "2026-02-17 10:34:00.064 -0600\tInformation\t148\tserver\t\
                        Schedule \"Nightly\" running.";
        let entry = parse_entry(7, observed.as_bytes(), false);
        assert_eq!(
            (
                entry.timestamp.as_str(),
                entry.level.as_str(),
                entry.number.as_str(),
                entry.host.as_str(),
                entry.message.as_str(),
            ),
            (
                "2026-02-17 10:34:00.064 -0600",
                "Information",
                "148",
                "server",
                "Schedule \"Nightly\" running."
            )
        );
        let time: jiff::Timestamp = "2026-02-17T16:34:00.064Z".parse().unwrap();
        assert_eq!(entry.time, Some(SystemTime::from(time)));

        // Names with tabs and line breaks cannot add fields or lines, and
        // the extension is left out as FileMaker Server leaves it out.
        let event = Event::file_opened("Tab\there\nnext.fmp12", false);
        let zoned = time.to_zoned(jiff::tz::TimeZone::fixed(jiff::tz::offset(-6)));
        let line = event.line(&zoned, "server");
        assert_eq!(
            line,
            "2026-02-17 10:34:00.064 -0600\tInformation\t184\tserver\t\
             Opened database \"Tab\\there\\nnext\".\n"
        );

        // Lines past the size limit rename the log, the full file ending
        // with 226 and the new one starting with 228, as FileMaker
        // Server's do; a second rename replaces the first -old file.
        let mut writer = writer(&folder, 400);
        for n in 0..12 {
            let event = Event::file_closed(&format!("File {n}.fmp12"));
            writer.write(Log::Event, &event.line(&zoned, "server"));
        }
        writer.write(Log::Access, &line);
        let numbers = |name: &str| -> Vec<String> {
            read_all(&folder.join(name))
                .into_iter()
                .rev()
                .map(|entry| format!("{} {}", entry.number, entry.message))
                .collect()
        };
        let closed = |n: u32| format!("168 Database \"File {n}\" closed.");
        let renamed = "226 This log file reached maximum size, and was renamed. A new log \
                       file was created.";
        let started =
            "228 The previous log file reached maximum size, and was renamed to \"Event-old.log\". ";
        assert_eq!(
            numbers("Event-old.log"),
            [
                started.to_owned(),
                closed(6),
                closed(7),
                closed(8),
                closed(9),
                renamed.to_owned()
            ]
        );
        assert_eq!(
            numbers("Event.log"),
            [started.to_owned(), closed(10), closed(11)]
        );
        assert_eq!(
            numbers("Access.log"),
            ["184 Opened database \"Tab\\there\\nnext\"."]
        );

        // A line still being written is left for a later page.
        std::fs::write(folder.join("Access.log"), format!("{line}2026-02-17 10:3")).unwrap();
        let entries = read_all(&folder.join("Access.log"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].offset, 0);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// A reader that counts the bytes read from it.
    struct Counted<'a>(std::io::Cursor<&'a [u8]>, u64);

    impl Read for Counted<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.0.read(buf)?;
            self.1 += n as u64;
            Ok(n)
        }
    }

    impl Seek for Counted<'_> {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.0.seek(pos)
        }
    }

    /// One page reads at most its budget however long a line is: a line
    /// longer than the budget comes in cut parts, newest part first, each
    /// page going on where the last stopped, and the lines around it whole.
    #[test]
    fn a_page_reads_a_bounded_part_of_the_file() {
        let budget = 100_000;
        let long = "x".repeat(250_000);
        let text = format!("first\n{long}\nlast\n");
        let read = pages(&text, budget);
        // "last" whole, then the long line's parts from its end, each kept
        // to its first MAX_LOG_LINE bytes, then its start and "first".
        let (len, step, line) = (text.len() as u64, budget - 1, MAX_LOG_LINE);
        assert_eq!(
            read,
            [
                vec![(len - 5, 4, false), (len - step, line, true)],
                vec![(len - 2 * step, line, true)],
                vec![(6, line, true), (0, 5, false)],
            ]
        );

        // A line still being written that is longer than the budget comes
        // in cut parts too, its start included; one within it waits.
        let text = format!("done\n{}", "y".repeat(budget as usize + 10));
        let len = text.len() as u64;
        assert_eq!(
            pages(&text, budget),
            [
                vec![(len - step, line, true)],
                vec![(5, (len - step - 5) as usize, true), (0, 4, false)],
            ]
        );
        assert_eq!(pages("done\nyy", budget), [vec![(0, 4, false)]]);
    }

    /// The pages of `text` read with `budget`, each as its entries'
    /// offsets, message lengths and cut marks; no page reads more than
    /// `budget` bytes.
    fn pages(text: &str, budget: u64) -> Vec<Vec<(u64, usize, bool)>> {
        let mut file = Counted(std::io::Cursor::new(text.as_bytes()), 0);
        let len = text.len() as u64;
        let mut pages = Vec::new();
        let mut before = None;
        loop {
            file.1 = 0;
            let page = read_page(&mut file, len, before, 10, budget).unwrap();
            assert!(file.1 <= budget, "a page read {} bytes", file.1);
            pages.push(
                page.entries
                    .iter()
                    .map(|e| (e.offset, e.message.len(), e.cut))
                    .collect(),
            );
            match page.next {
                Some(next) => before = Some(next),
                None => return pages,
            }
        }
    }
}
