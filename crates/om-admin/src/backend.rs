//! What the console asks of the server it administers.
//!
//! The model follows FileMaker Server's Admin API (`docs/re/admin-console.md`
//! sections 7 and 8), so the console's views and the Admin API read the same
//! state. `om-server` implements it over its `ServerHandle`;
//! [`crate::MockBackend`] is an in-memory one.
//!
//! A server that lacks a feature answers [`BackendError::Unsupported`] for
//! it (each method's default), and the console shows that view as not
//! available rather than empty (`docs/admin-console.md`, "Scope").

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// The server the console administers. Called from the console's own
/// threads; each call should return quickly, without waiting for a file a
/// request is using.
pub trait Backend: Send + Sync + 'static {
    // ---- The top bar.

    /// The server's alerts that the administrator has not acknowledged,
    /// newest first: the top bar's bell lists them.
    fn alerts(&self) -> Vec<Alert>;

    /// Acknowledge alert `id`, which removes it from [`Backend::alerts`].
    /// False when there is no such alert.
    fn acknowledge_alert(&self, id: u64) -> bool;

    // ---- The server (Dashboard, General Settings; `/server/metadata`,
    // `/server/status`).

    /// The server's identity and state.
    fn server(&self) -> ServerInfo;

    // ---- Databases and clients (Dashboard, Databases; `/databases`,
    // `/clients`).

    /// The hosted files, in the server's order.
    fn databases(&self) -> Vec<Database>;

    /// The connected clients, oldest session first.
    fn clients(&self) -> Vec<Client>;

    /// Disconnect client `id`: at once without a notice, or after asking it
    /// to close its files and waiting the notice's grace time
    /// (`DELETE /clients/{id}`).
    fn disconnect_client(&self, id: u32, notice: Option<&Notice>) -> Result<(), BackendError> {
        let _ = (id, notice);
        Err(BackendError::Unsupported)
    }

    /// Show client `id` a message (`POST /clients/{id}/message`).
    fn send_message(&self, id: u32, text: &str) -> Result<(), BackendError> {
        let _ = (id, text);
        Err(BackendError::Unsupported)
    }

    /// Open, close, pause or resume hosted file `id`
    /// (`PATCH /databases/{id}`).
    fn change_database(&self, id: u32, change: &DatabaseChange) -> Result<(), BackendError> {
        let _ = (id, change);
        Err(BackendError::Unsupported)
    }

    // ---- Settings (General Settings; `/server/config/*`).

    /// Whether Block New Users is on: new clients cannot open hosted files.
    fn block_new_users(&self) -> Result<bool, BackendError> {
        Err(BackendError::Unsupported)
    }

    /// Turn Block New Users on or off.
    fn set_block_new_users(&self, on: bool) -> Result<(), BackendError> {
        let _ = on;
        Err(BackendError::Unsupported)
    }

    // ---- Configuration: Folders and SSL Certificate.

    /// The folders the server hosts files from and backs them up to.
    fn folders(&self) -> Result<Folders, BackendError> {
        Err(BackendError::Unsupported)
    }

    /// The certificate chain the server presents, leaf first, in DER.
    fn certificate(&self) -> Result<Vec<Vec<u8>>, BackendError> {
        Err(BackendError::Unsupported)
    }

    // ---- Schedules (Configuration > Schedules; `/schedules`).

    /// The schedules, in the server's order.
    fn schedules(&self) -> Result<Vec<Schedule>, BackendError> {
        Err(BackendError::Unsupported)
    }

    /// Run schedule `id` now, as Run Now does.
    fn run_schedule(&self, id: u32) -> Result<(), BackendError> {
        let _ = id;
        Err(BackendError::Unsupported)
    }

    /// Enable or disable schedule `id`.
    fn enable_schedule(&self, id: u32, enabled: bool) -> Result<(), BackendError> {
        let _ = (id, enabled);
        Err(BackendError::Unsupported)
    }

    // ---- Backups (the Backups tab).

    /// The backups in the backup folder and whether one is running.
    fn backups(&self) -> Result<Backups, BackendError> {
        Err(BackendError::Unsupported)
    }

    /// Run backup schedule `schedule` now (Back Up Now).
    fn back_up_now(&self, schedule: u32) -> Result<(), BackendError> {
        let _ = schedule;
        Err(BackendError::Unsupported)
    }

    /// Preserve backup `name` (its folder name) from automatic deletion, or
    /// stop preserving it.
    fn preserve_backup(&self, name: &str, preserved: bool) -> Result<(), BackendError> {
        let _ = (name, preserved);
        Err(BackendError::Unsupported)
    }

    /// Give backup `name` a new label.
    fn label_backup(&self, name: &str, label: &str) -> Result<(), BackendError> {
        let _ = (name, label);
        Err(BackendError::Unsupported)
    }

    /// Delete preserved backup `name`.
    fn delete_backup(&self, name: &str) -> Result<(), BackendError> {
        let _ = name;
        Err(BackendError::Unsupported)
    }

    /// How backup schedule `schedule` repeats and how many backups it
    /// keeps: the Backups tab's Repeat and Keep.
    fn backup_plan(&self, schedule: u32) -> Result<BackupPlan, BackendError> {
        let _ = schedule;
        Err(BackendError::Unsupported)
    }

    // ---- Logs (the Logs tab).

    /// The server's log files.
    fn log_files(&self) -> Result<Vec<LogFile>, BackendError> {
        Err(BackendError::Unsupported)
    }

    /// A page of log file `name`'s entries, newest first.
    fn read_log(&self, name: &str, page: LogPage) -> Result<LogEntries, BackendError> {
        let _ = (name, page);
        Err(BackendError::Unsupported)
    }

    // ---- System statistics.

    /// The folder whose file system the Dashboard's Volume Status shows: the
    /// one that holds the hosted files.
    fn data_folder(&self) -> Option<PathBuf> {
        None
    }
}

/// Why the server did not do what the console asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendError {
    /// The server does not have this feature.
    Unsupported,
    /// No such client, file, schedule or backup (the Admin API's 1700).
    NotFound,
    /// Not in this state, such as closing a file that is closed (1709); the
    /// text says why.
    InvalidState(String),
    /// The administrator's input is not accepted; the text says why.
    Invalid(String),
    /// The server tried and failed; the text says why.
    Failed(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::Unsupported => f.write_str("OpenMaker Server does not do this yet."),
            BackendError::NotFound => f.write_str("It no longer exists."),
            BackendError::InvalidState(why)
            | BackendError::Invalid(why)
            | BackendError::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for BackendError {}

/// Something the administrator should know about, such as a hosted file
/// that failed to save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    /// Unique among the server's alerts.
    pub id: u64,
    /// When it happened.
    pub time: SystemTime,
    pub severity: Severity,
    /// One plain-text sentence or two.
    pub message: String,
}

/// How serious an [`Alert`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Critical,
    Warning,
    Information,
}

/// The server as the administrator sees it (`/server/metadata`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    /// The name clients see.
    pub name: String,
    /// Its id, in hex.
    pub id: String,
    /// The version it reports, e.g. `OpenMaker Server 0.0.0`.
    pub version: String,
    /// When it started.
    pub started_at: SystemTime,
    /// Where it listens for FileMaker clients (port 5003 by default). An
    /// unspecified address means every address of the machine.
    pub address: SocketAddr,
    /// The machine's own addresses clients can reach it by, for "Server IP
    /// Addresses"; empty when the server does not know them.
    pub ip_addresses: Vec<String>,
}

/// A hosted file (`/databases`: `databases[]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Database {
    /// The server's id for it.
    pub id: u32,
    /// Its name as clients open it, with the extension (`Sample.fmp12`).
    pub name: String,
    /// The folder that holds it.
    pub folder: PathBuf,
    /// Its size on disk in bytes, when known.
    pub size: Option<u64>,
    pub status: DatabaseStatus,
    /// How many connected clients have it open.
    pub clients: usize,
    /// Whether it is encrypted at rest.
    pub encrypted: bool,
    /// Why it is served read-only, when it is: no client's change is
    /// stored. OpenMaker's own; FileMaker Server's console shows no such
    /// mark that has been seen.
    pub read_only: Option<String>,
}

/// What state a hosted file is in, with FileMaker Server's names
/// (`docs/re/admin-console.md` 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseStatus {
    Normal,
    Opening,
    Closing,
    Closed,
    Pausing,
    Paused,
    Resuming,
}

impl DatabaseStatus {
    /// The Admin API's name: `NORMAL`, `CLOSED`, ...
    pub fn api_name(self) -> &'static str {
        match self {
            DatabaseStatus::Normal => "NORMAL",
            DatabaseStatus::Opening => "OPENING",
            DatabaseStatus::Closing => "CLOSING",
            DatabaseStatus::Closed => "CLOSED",
            DatabaseStatus::Pausing => "PAUSING",
            DatabaseStatus::Paused => "PAUSED",
            DatabaseStatus::Resuming => "RESUMING",
        }
    }

    /// The console's word for it: Open, Closed, Paused, or the transition.
    pub fn label(self) -> &'static str {
        match self {
            DatabaseStatus::Normal => "Open",
            DatabaseStatus::Opening => "Opening",
            DatabaseStatus::Closing => "Closing",
            DatabaseStatus::Closed => "Closed",
            DatabaseStatus::Pausing => "Pausing",
            DatabaseStatus::Paused => "Paused",
            DatabaseStatus::Resuming => "Resuming",
        }
    }
}

/// What to do with a hosted file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatabaseChange {
    Open,
    /// Close it, asking its clients to close it first.
    Close(Notice),
    Pause,
    Resume,
}

/// What a client is told before it is disconnected or a file it uses is
/// closed, and how long it has to comply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// At most 200 characters; may be empty.
    pub message: String,
    pub grace: Duration,
}

/// A connected client (`/clients`: `clients[]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    /// The server's id for it.
    pub id: u32,
    /// The name it gave when it signed in, which FileMaker Server shows as
    /// "Session Identifier"; `None` before then.
    pub session_name: Option<String>,
    /// Its computer's name.
    pub machine_name: String,
    /// Its address.
    pub address: SocketAddr,
    /// FileMaker's application type number (`ApplicationInfo::AppType`,
    /// `docs/re/calc-functions.md` 6): 1 for FileMaker Pro.
    pub app_type: u8,
    /// Its version string, e.g. `Pro 26.0.3`.
    pub version: String,
    /// When its session started.
    pub connected_at: SystemTime,
    /// The files it has open.
    pub files: Vec<ClientFile>,
    /// It is being disconnected.
    pub disconnecting: bool,
}

/// A file a client has open (`guestFiles[]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientFile {
    /// The file's id ([`Database::id`]).
    pub id: u32,
    /// Its name as clients open it.
    pub name: String,
    /// The account signed in to it, when one is.
    pub account: Option<String>,
}

/// The server's folders (`/server/currentfoldersettings`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folders {
    /// The folders that hold hosted files.
    pub databases: Vec<PathBuf>,
    /// Where backups go, when the server makes them.
    pub backups: Option<PathBuf>,
}

/// A schedule (`/schedules`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    /// The server's id for it, 2 or more: FileMaker Server reserves
    /// schedule id 1, which the Admin API refuses (`/schedules/{id}`).
    pub id: u32,
    pub name: String,
    pub kind: ScheduleKind,
    /// The file a FileMaker script schedule runs in, or what a backup
    /// schedule backs up, as the console words it.
    pub target: Option<String>,
    /// The account a FileMaker script schedule runs as.
    pub account: Option<String>,
    pub enabled: bool,
    /// When it runs next on its own.
    pub next_run: Option<SystemTime>,
    /// Its latest run that ended.
    pub last_run: Option<ScheduleRun>,
    /// A run is executing now.
    pub running: bool,
}

/// What a schedule does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleKind {
    FileMakerScript,
    Backup,
}

impl ScheduleKind {
    /// FileMaker Server's name for it in the Schedules list.
    pub fn label(self) -> &'static str {
        match self {
            ScheduleKind::FileMakerScript => "FileMaker Script",
            ScheduleKind::Backup => "Backup",
        }
    }
}

/// How a schedule's run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleRun {
    /// When it started.
    pub started_at: SystemTime,
    pub succeeded: bool,
    /// Why it failed, or the script's last error.
    pub detail: Option<String>,
}

/// The backups and the backup activity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backups {
    /// The backups, newest first.
    pub items: Vec<Backup>,
    /// The backup schedule whose run is executing now.
    pub running: Option<u32>,
}

/// One backup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    /// Its folder's name, which identifies it.
    pub name: String,
    /// The schedule that made it.
    pub schedule: String,
    /// When it was made.
    pub time: SystemTime,
    /// FileMaker Server's label for it, such as `Auto-backup`.
    pub label: String,
    pub preserved: bool,
    /// Its size on disk in bytes, when known.
    pub size: Option<u64>,
}

/// How a backup schedule runs, as the Backups tab's sidebar shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupPlan {
    /// When it repeats, as the console words it: `Daily at Midnight`.
    pub repeat: String,
    /// How many of its backups it keeps before deleting the oldest (the
    /// schedule's "Number of Backups to Keep").
    pub keep: u32,
}

/// A log file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFile {
    /// Its file name, e.g. `Event.log`.
    pub name: String,
    pub size: u64,
    pub modified: SystemTime,
}

/// Which entries of a log file to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogPage {
    /// Entries to skip, counted from the newest.
    pub skip: usize,
    /// Entries to return at most.
    pub limit: usize,
}

/// A page of a log file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntries {
    /// Newest first.
    pub entries: Vec<LogEntry>,
    /// Older entries follow these.
    pub more: bool,
}

/// One log entry, as it is written in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    /// Its time stamp as written.
    pub timestamp: String,
    /// `Information`, `Warning` or `Error`.
    pub level: String,
    /// The number FileMaker writes in its Error column, when there is one.
    pub code: Option<i64>,
    /// The machine that wrote it.
    pub host: String,
    pub message: String,
}
