//! An in-memory [`Backend`] for tests and for working on the console without
//! a server. Its actions change its state as a server's would, at once:
//! there are no grace times or transitions.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use crate::backend::{
    Alert, Backend, BackendError, Backup, BackupPlan, Backups, Client, ClientFile, Database,
    DatabaseChange, DatabaseStatus, Folders, LogEntries, LogEntry, LogFile, LogPage, Notice,
    Schedule, ScheduleKind, ScheduleRun, ServerInfo, Severity,
};

/// A [`Backend`] holding its state in memory.
#[derive(Debug)]
pub struct MockBackend {
    state: Mutex<MockState>,
}

/// Everything a [`MockBackend`] answers with. A `None` feature answers
/// [`BackendError::Unsupported`].
#[derive(Clone, Debug)]
pub struct MockState {
    pub alerts: Vec<Alert>,
    pub server: ServerInfo,
    pub databases: Vec<Database>,
    pub clients: Vec<Client>,
    pub block_new_users: Option<bool>,
    pub folders: Option<Folders>,
    pub certificate: Option<Vec<Vec<u8>>>,
    pub schedules: Option<Vec<Schedule>>,
    pub backups: Option<Backups>,
    /// Each backup schedule's plan, by the schedule's id.
    pub backup_plans: Vec<(u32, BackupPlan)>,
    /// Each log file with its entries, newest first.
    pub logs: Option<Vec<(LogFile, Vec<LogEntry>)>>,
    pub data_folder: Option<PathBuf>,
    /// Messages sent with [`Backend::send_message`]: client id and text.
    pub messages: Vec<(u32, String)>,
    /// Notices given with [`Backend::disconnect_client`] and closing files.
    pub notices: Vec<Notice>,
}

impl MockBackend {
    /// A backend with this state.
    pub fn new(state: MockState) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }

    /// A server with no files, clients or alerts, and every feature
    /// unsupported.
    pub fn empty() -> Self {
        Self::new(MockState::empty())
    }

    /// A backend with these alerts, newest first, and otherwise empty.
    pub fn with_alerts(alerts: Vec<Alert>) -> Self {
        Self::new(MockState {
            alerts,
            ..MockState::empty()
        })
    }

    /// Labeled sample data for every view, for design work and tests.
    pub fn sample() -> Self {
        Self::new(MockState::sample())
    }

    /// The state, to inspect or change.
    pub fn state(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::empty()
    }
}

fn server_info(now: SystemTime) -> ServerInfo {
    ServerInfo {
        name: "sample-server".into(),
        id: "5A3F0C2B9E7D41A8B6C4D2E0F1A3B5C7".into(),
        version: "OpenMaker Server 0.0.0".into(),
        started_at: now - Duration::from_secs(26 * 3600 + 17 * 60),
        address: SocketAddr::from(([0, 0, 0, 0], 5003)),
        ip_addresses: vec!["192.0.2.10".into()],
    }
}

impl MockState {
    /// A server with no files, clients or alerts, and every feature
    /// unsupported.
    pub fn empty() -> Self {
        Self {
            alerts: Vec::new(),
            server: server_info(SystemTime::now()),
            databases: Vec::new(),
            clients: Vec::new(),
            block_new_users: None,
            folders: None,
            certificate: None,
            schedules: None,
            backups: None,
            backup_plans: Vec::new(),
            logs: None,
            data_folder: None,
            messages: Vec::new(),
            notices: Vec::new(),
        }
    }

    /// Labeled sample data for every view.
    pub fn sample() -> Self {
        let now = SystemTime::now();
        let ago = |secs: u64| now - Duration::from_secs(secs);
        let folder = PathBuf::from("/srv/openmaker/Databases");
        let database = |id: u32, name: &str, sub: &str, size: u64, status, clients| Database {
            id,
            name: name.into(),
            folder: if sub.is_empty() {
                folder.clone()
            } else {
                folder.join(sub)
            },
            size: Some(size),
            status,
            clients,
            encrypted: false,
        };
        let file = |id: u32, name: &str, account: Option<&str>| ClientFile {
            id,
            name: name.into(),
            account: account.map(Into::into),
        };
        let client = |id: u32, name: &str, machine: &str, ip: [u8; 4], files, secs| Client {
            id,
            session_name: Some(name.into()),
            machine_name: machine.into(),
            address: SocketAddr::from((ip, 50_000 + (id as u16 % 1000))),
            app_type: 1,
            version: "Pro 26.0.3".into(),
            connected_at: ago(secs),
            files,
            disconnecting: false,
        };
        let today = jiff::Zoned::now().start_of_day().ok();
        let next_midnight = today
            .as_ref()
            .and_then(|t| t.tomorrow().ok())
            .map_or(now, |z| SystemTime::from(z.timestamp()));
        Self {
            alerts: vec![
                Alert {
                    id: 2,
                    time: ago(5 * 60),
                    severity: Severity::Critical,
                    message: "Sample alert: the hosted file \"Invoices.fmp12\" could not be \
                              saved: no space left on device."
                        .into(),
                },
                Alert {
                    id: 1,
                    time: ago(3 * 3600),
                    severity: Severity::Warning,
                    message: "Sample alert: the SSL certificate expires in 12 days.".into(),
                },
            ],
            server: server_info(now),
            databases: vec![
                database(
                    1,
                    "Contacts.fmp12",
                    "",
                    3_276_800,
                    DatabaseStatus::Normal,
                    3,
                ),
                database(
                    2,
                    "Invoices.fmp12",
                    "",
                    9_437_184,
                    DatabaseStatus::Normal,
                    1,
                ),
                database(
                    3,
                    "Inventory.fmp12",
                    "Warehouse",
                    1_048_576,
                    DatabaseStatus::Paused,
                    0,
                ),
                database(
                    4,
                    "Archive 2024.fmp12",
                    "Warehouse",
                    52_428_800,
                    DatabaseStatus::Closed,
                    0,
                ),
            ],
            clients: vec![
                client(
                    11,
                    "Sample User 1",
                    "Sample-Laptop-1",
                    [192, 0, 2, 21],
                    vec![file(1, "Contacts.fmp12", Some("Admin"))],
                    3 * 3600,
                ),
                client(
                    12,
                    "Sample User 2",
                    "Sample-Desktop-2",
                    [192, 0, 2, 22],
                    vec![
                        file(1, "Contacts.fmp12", Some("Sales")),
                        file(2, "Invoices.fmp12", Some("Sales")),
                    ],
                    25 * 60,
                ),
                Client {
                    app_type: 5,
                    version: "Go 26.0.1".into(),
                    ..client(
                        13,
                        "Sample User 3",
                        "Sample-Tablet-3",
                        [192, 0, 2, 23],
                        vec![file(1, "Contacts.fmp12", Some("Field"))],
                        7 * 60,
                    )
                },
            ],
            block_new_users: Some(false),
            folders: Some(Folders {
                databases: vec![folder.clone()],
                backups: Some("/srv/openmaker/Backups".into()),
            }),
            certificate: None,
            schedules: Some(vec![
                Schedule {
                    id: 2,
                    name: "Nightly Totals".into(),
                    kind: ScheduleKind::FileMakerScript,
                    target: Some("file:Invoices.fmp12".into()),
                    account: Some("Admin".into()),
                    enabled: true,
                    next_run: Some(now + Duration::from_secs(5 * 3600)),
                    last_run: Some(ScheduleRun {
                        started_at: ago(19 * 3600),
                        succeeded: true,
                        detail: None,
                    }),
                    running: false,
                },
                Schedule {
                    id: 3,
                    name: "Daily".into(),
                    kind: ScheduleKind::Backup,
                    target: Some("All Databases".into()),
                    account: None,
                    enabled: true,
                    next_run: Some(next_midnight),
                    last_run: Some(ScheduleRun {
                        started_at: ago(20 * 3600),
                        succeeded: true,
                        detail: None,
                    }),
                    running: false,
                },
                Schedule {
                    id: 4,
                    name: "Weekly Cleanup".into(),
                    kind: ScheduleKind::FileMakerScript,
                    target: Some("file:Contacts.fmp12".into()),
                    account: Some("Admin".into()),
                    enabled: false,
                    next_run: None,
                    last_run: Some(ScheduleRun {
                        started_at: ago(6 * 86_400),
                        succeeded: false,
                        detail: Some(
                            "Sample failure: script error 101 (record is missing).".into(),
                        ),
                    }),
                    running: false,
                },
            ]),
            backups: Some(Backups {
                items: sample_backups(now, today.as_ref()),
                running: None,
            }),
            backup_plans: vec![(
                3,
                BackupPlan {
                    repeat: "Daily at Midnight".into(),
                    keep: 7,
                },
            )],
            logs: Some(sample_logs(now)),
            data_folder: Some(folder),
            messages: Vec::new(),
            notices: Vec::new(),
        }
    }
}

/// `time` in the server's time zone, as `format` writes it.
fn local(time: SystemTime, format: &str) -> String {
    jiff::Timestamp::try_from(time)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime(format)
                .to_string()
        })
        .unwrap_or_default()
}

/// A backup's folder name, as FileMaker Server names one: the schedule's
/// name and when the run started (`docs/re/admin-console.md` 4.3).
fn backup_name(schedule: &str, time: SystemTime) -> String {
    format!("{schedule}_{}", local(time, "%Y-%m-%d_%H%M"))
}

/// The sample's backups, newest first: the "Daily" schedule's last seven
/// midnight runs (one preserved), a Back Up Now, and two of a "Weekly"
/// schedule since deleted.
fn sample_backups(now: SystemTime, today: Option<&jiff::Zoned>) -> Vec<Backup> {
    // Midnight `days` days ago, in the server's time zone.
    let midnight = |days: i64| {
        today
            .and_then(|t| t.checked_sub(jiff::Span::new().days(days)).ok())
            .map_or(now, |z| SystemTime::from(z.timestamp()))
    };
    let backup = |schedule: &str, time: SystemTime, label: &str, preserved: bool| Backup {
        name: backup_name(schedule, time),
        schedule: schedule.into(),
        time,
        label: label.into(),
        preserved,
        size: Some(66_191_360),
    };
    let mut items: Vec<Backup> = (0..7)
        .map(|k| backup("Daily", midnight(k), "Auto-backup", k == 2))
        .collect();
    items.push(backup(
        "Daily",
        now - Duration::from_secs(2 * 3600 + 5 * 60),
        "On-demand",
        false,
    ));
    items.push(backup("Weekly", midnight(9), "Before upgrade", true));
    items.push(backup("Weekly", midnight(16), "Auto-backup", false));
    items.sort_by_key(|b| std::cmp::Reverse(b.time));
    items
}

/// The sample's logs, newest first, in FileMaker Server's wording
/// (`docs/re/admin-console.md` 4.9): Event.log with a script schedule's run
/// every five minutes, three Log Viewer pages of them, and Access.log with
/// clients' sessions.
fn sample_logs(now: SystemTime) -> Vec<(LogFile, Vec<LogEntry>)> {
    let entry = |time: SystemTime, level: &str, code: i64, message: String| LogEntry {
        timestamp: local(time, "%Y-%m-%d %H:%M:%S%.3f %z"),
        level: level.into(),
        code: Some(code),
        host: "sample-server".into(),
        message,
    };
    let ms = Duration::from_millis;
    let mut events = Vec::new();
    for run in 0..108u64 {
        let start = now - Duration::from_secs(90 + run * 300);
        let schedule = "Schedule \"Refresh Totals\"";
        let next = local(start + Duration::from_secs(300), "%m/%d/%Y %I:%M %p");
        events.push(entry(
            start + ms(1300),
            "Information",
            126,
            format!("{schedule} scheduled for {next}."),
        ));
        events.push(if run % 9 == 4 {
            entry(
                start + ms(1200),
                "Information",
                644,
                format!("{schedule} completed; last scripting error (401)."),
            )
        } else {
            entry(
                start + ms(1200),
                "Information",
                150,
                format!("{schedule} completed."),
            )
        });
        events.push(entry(
            start + ms(40),
            "Information",
            689,
            format!("{schedule} has started FileMaker script \"Update Totals\"."),
        ));
        events.push(entry(
            start,
            "Information",
            148,
            format!("{schedule} running."),
        ));
    }
    let mut access = Vec::new();
    for session in 0..12u64 {
        let n = session % 3 + 1;
        let client = format!("Sample User {n} (Sample-Laptop-{n}) [192.0.2.2{n}]");
        let start = now - Duration::from_secs(600 + session * 1500);
        let end = start + Duration::from_secs(900);
        if session == 5 {
            access.push(entry(
                end,
                "Warning",
                30,
                format!("Client \"{client}\" no longer responding; connection closed. (10)"),
            ));
        } else {
            access.push(entry(
                end + ms(2),
                "Information",
                22,
                format!("Client \"{client}\" closing a connection."),
            ));
            access.push(entry(
                end,
                "Information",
                98,
                format!("Client \"{client}\" closing database \"Contacts\" as \"Admin\"."),
            ));
        }
        access.push(entry(
            start + ms(1),
            "Information",
            94,
            format!("Client \"{client}\" opening database \"Contacts\" as \"Admin\"."),
        ));
        access.push(entry(
            start,
            "Information",
            638,
            format!(
                "Client \"Sample User {n}\" opening a connection from \"Sample-Laptop-{n} \
                 (192.0.2.2{n})\" using \"Pro 26.0.3 [fmapp]\"."
            ),
        ));
    }
    access.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    let file = |name: &str, entries: Vec<LogEntry>| {
        let size = entries.iter().map(|e| e.message.len() as u64 + 60).sum();
        (
            LogFile {
                name: name.into(),
                size,
                modified: now,
            },
            entries,
        )
    };
    vec![file("Access.log", access), file("Event.log", events)]
}

fn unsupported<T: Clone>(value: &Option<T>) -> Result<T, BackendError> {
    value.clone().ok_or(BackendError::Unsupported)
}

impl Backend for MockBackend {
    fn alerts(&self) -> Vec<Alert> {
        self.state().alerts.clone()
    }

    fn acknowledge_alert(&self, id: u64) -> bool {
        let mut state = self.state();
        let before = state.alerts.len();
        state.alerts.retain(|a| a.id != id);
        state.alerts.len() != before
    }

    fn server(&self) -> ServerInfo {
        self.state().server.clone()
    }

    fn databases(&self) -> Vec<Database> {
        self.state().databases.clone()
    }

    fn clients(&self) -> Vec<Client> {
        self.state().clients.clone()
    }

    fn disconnect_client(&self, id: u32, notice: Option<&Notice>) -> Result<(), BackendError> {
        let mut state = self.state();
        let index = state
            .clients
            .iter()
            .position(|c| c.id == id)
            .ok_or(BackendError::NotFound)?;
        let client = state.clients.remove(index);
        for file in &client.files {
            if let Some(database) = state.databases.iter_mut().find(|d| d.id == file.id) {
                database.clients = database.clients.saturating_sub(1);
            }
        }
        if let Some(notice) = notice {
            state.notices.push(notice.clone());
        }
        Ok(())
    }

    fn send_message(&self, id: u32, text: &str) -> Result<(), BackendError> {
        let mut state = self.state();
        if !state.clients.iter().any(|c| c.id == id) {
            return Err(BackendError::NotFound);
        }
        state.messages.push((id, text.to_owned()));
        Ok(())
    }

    fn change_database(&self, id: u32, change: &DatabaseChange) -> Result<(), BackendError> {
        let mut state = self.state();
        let MockState {
            databases,
            clients,
            notices,
            ..
        } = &mut *state;
        let database = databases
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or(BackendError::NotFound)?;
        use DatabaseStatus::{Closed, Normal, Paused};
        let (from, to) = match change {
            DatabaseChange::Open => (Closed, Normal),
            DatabaseChange::Close(_) => (database.status, Closed),
            DatabaseChange::Pause => (Normal, Paused),
            DatabaseChange::Resume => (Paused, Normal),
        };
        if database.status != from || (to == Closed && from == Closed) {
            return Err(BackendError::InvalidState(format!(
                "\"{}\" is {}.",
                database.name,
                database.status.label().to_lowercase()
            )));
        }
        database.status = to;
        if let DatabaseChange::Close(notice) = change {
            notices.push(notice.clone());
            database.clients = 0;
            for client in clients.iter_mut() {
                client.files.retain(|f| f.id != id);
            }
        }
        Ok(())
    }

    fn block_new_users(&self) -> Result<bool, BackendError> {
        unsupported(&self.state().block_new_users)
    }

    fn set_block_new_users(&self, on: bool) -> Result<(), BackendError> {
        let mut state = self.state();
        let setting = state
            .block_new_users
            .as_mut()
            .ok_or(BackendError::Unsupported)?;
        *setting = on;
        Ok(())
    }

    fn folders(&self) -> Result<Folders, BackendError> {
        unsupported(&self.state().folders)
    }

    fn certificate(&self) -> Result<Vec<Vec<u8>>, BackendError> {
        unsupported(&self.state().certificate)
    }

    fn schedules(&self) -> Result<Vec<Schedule>, BackendError> {
        unsupported(&self.state().schedules)
    }

    fn run_schedule(&self, id: u32) -> Result<(), BackendError> {
        let mut state = self.state();
        let schedule = state
            .schedules
            .as_mut()
            .ok_or(BackendError::Unsupported)?
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(BackendError::NotFound)?;
        schedule.last_run = Some(ScheduleRun {
            started_at: SystemTime::now(),
            succeeded: true,
            detail: None,
        });
        Ok(())
    }

    fn enable_schedule(&self, id: u32, enabled: bool) -> Result<(), BackendError> {
        let mut state = self.state();
        let schedule = state
            .schedules
            .as_mut()
            .ok_or(BackendError::Unsupported)?
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(BackendError::NotFound)?;
        schedule.enabled = enabled;
        // As the server's scheduler: a disabled schedule does not run on its
        // own; enabled again, the mock runs it a day later.
        schedule.next_run = enabled.then(|| {
            schedule
                .next_run
                .unwrap_or_else(|| SystemTime::now() + Duration::from_secs(24 * 3600))
        });
        Ok(())
    }

    fn backups(&self) -> Result<Backups, BackendError> {
        unsupported(&self.state().backups)
    }

    fn back_up_now(&self, schedule: u32) -> Result<(), BackendError> {
        let mut state = self.state();
        let name = state
            .schedules
            .as_ref()
            .and_then(|s| s.iter().find(|s| s.id == schedule))
            .filter(|s| s.kind == ScheduleKind::Backup)
            .map(|s| s.name.clone())
            .ok_or(BackendError::NotFound)?;
        let backups = state.backups.as_mut().ok_or(BackendError::Unsupported)?;
        let time = SystemTime::now();
        // A name no backup has: backups are found by name. A second run in
        // the same minute gets a folder of its own.
        let mut folder = backup_name(&name, time);
        let mut n = 1;
        while backups.items.iter().any(|b| b.name == folder) {
            n += 1;
            folder = format!("{}_{n}", backup_name(&name, time));
        }
        backups.items.insert(
            0,
            Backup {
                name: folder,
                schedule: name,
                time,
                label: "On-demand".into(),
                preserved: false,
                size: Some(66_191_360),
            },
        );
        Ok(())
    }

    fn preserve_backup(&self, name: &str, preserved: bool) -> Result<(), BackendError> {
        self.with_backup(name, |b| b.preserved = preserved)
    }

    fn label_backup(&self, name: &str, label: &str) -> Result<(), BackendError> {
        self.with_backup(name, |b| b.label = label.to_owned())
    }

    fn delete_backup(&self, name: &str) -> Result<(), BackendError> {
        let mut state = self.state();
        let backups = state.backups.as_mut().ok_or(BackendError::Unsupported)?;
        let index = backups
            .items
            .iter()
            .position(|b| b.name == name)
            .ok_or(BackendError::NotFound)?;
        if !backups.items[index].preserved {
            return Err(BackendError::InvalidState(
                "Only a preserved backup can be deleted.".into(),
            ));
        }
        backups.items.remove(index);
        Ok(())
    }

    fn backup_plan(&self, schedule: u32) -> Result<BackupPlan, BackendError> {
        let state = self.state();
        if state.backups.is_none() {
            return Err(BackendError::Unsupported);
        }
        state
            .backup_plans
            .iter()
            .find(|(id, _)| *id == schedule)
            .map(|(_, plan)| plan.clone())
            .ok_or(BackendError::NotFound)
    }

    fn log_files(&self) -> Result<Vec<LogFile>, BackendError> {
        let state = self.state();
        let logs = state.logs.as_ref().ok_or(BackendError::Unsupported)?;
        Ok(logs.iter().map(|(file, _)| file.clone()).collect())
    }

    fn read_log(&self, name: &str, page: LogPage) -> Result<LogEntries, BackendError> {
        let state = self.state();
        let logs = state.logs.as_ref().ok_or(BackendError::Unsupported)?;
        let (_, entries) = logs
            .iter()
            .find(|(file, _)| file.name == name)
            .ok_or(BackendError::NotFound)?;
        let rest = entries.get(page.skip..).unwrap_or_default();
        let shown = rest.len().min(page.limit);
        Ok(LogEntries {
            entries: rest[..shown].to_vec(),
            more: rest.len() > shown,
        })
    }

    fn data_folder(&self) -> Option<PathBuf> {
        self.state().data_folder.clone()
    }
}

impl MockBackend {
    fn with_backup(
        &self,
        name: &str,
        change: impl FnOnce(&mut Backup),
    ) -> Result<(), BackendError> {
        let mut state = self.state();
        let backups = state.backups.as_mut().ok_or(BackendError::Unsupported)?;
        let backup = backups
            .items
            .iter_mut()
            .find(|b| b.name == name)
            .ok_or(BackendError::NotFound)?;
        change(backup);
        Ok(())
    }
}
