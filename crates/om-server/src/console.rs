//! The Admin Console in OpenMaker Server: `om_admin`'s console served
//! beside the RCFMP12 listener, reading and acting on the running server
//! through its [`ServerHandle`] (`docs/admin-console.md`).
//!
//! [`ServerBackend`] answers each of the console's questions from the
//! handle's snapshots, so a view never waits for a file a request is
//! using. What the server cannot do yet answers
//! [`BackendError::Unsupported`], and the console shows it as not
//! available.

use std::collections::BTreeSet;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use om_admin::{
    Alert, Backend, BackendError, Client, ClientFile, ConsoleConfig, ConsoleHandle, Database,
    DatabaseStatus, Folders, Notice, PasswordDigest, Schedule, ScheduleKind, ScheduleRun,
    ServerInfo,
};

use crate::admin::FileStatus;
use crate::server::ServerHandle;
use crate::tls::Identity;
use crate::{BackupFiles, ScheduleStatus, ScheduleTask};

/// The id of the first schedule in the `--schedules` file: FileMaker
/// Server reserves schedule id 1.
const FIRST_SCHEDULE_ID: u32 = 2;

/// Where the console listens unless told otherwise: this machine only, on
/// the port FileMaker Server's console used before it moved behind 443.
pub const DEFAULT_ADMIN_BIND: &str = "127.0.0.1:16000";

/// Start the Admin Console for the server `handle` administers, presenting
/// `identity` (the server's own certificate) on `bind`, for the account
/// `user` with the password `password`.
pub fn start_console(
    handle: ServerHandle,
    identity: &Identity,
    bind: SocketAddr,
    user: String,
    password: PasswordDigest,
) -> io::Result<ConsoleHandle> {
    let certificates: Vec<Vec<u8>> = identity.certs.iter().map(|c| c.to_vec()).collect();
    let backend = ServerBackend::new(handle, certificates);
    om_admin::Console::start(
        ConsoleConfig {
            bind,
            certificates: identity.certs.clone(),
            key: identity.key.clone_key(),
            user,
            password,
        },
        Arc::new(backend),
    )
}

/// The console's view of a running server.
pub struct ServerBackend {
    handle: ServerHandle,
    /// The certificate chain the server presents, leaf first, in DER.
    certificates: Vec<Vec<u8>>,
    /// The addresses clients can reach the server by, worked out once.
    ip_addresses: Vec<String>,
}

impl ServerBackend {
    pub fn new(handle: ServerHandle, certificates: Vec<Vec<u8>>) -> Self {
        let ip_addresses = reachable_addresses(handle.overview().address);
        Self {
            handle,
            certificates,
            ip_addresses,
        }
    }

    /// The schedules in the server's order, numbered from 2 (FileMaker
    /// Server reserves 1): the console's and the Admin API's schedule ids.
    fn schedule_by_id(&self, id: u32) -> Result<ScheduleStatus, BackendError> {
        let index = usize::try_from(id)
            .ok()
            .and_then(|id| id.checked_sub(FIRST_SCHEDULE_ID as usize))
            .ok_or(BackendError::NotFound)?;
        self.handle
            .schedules()
            .into_iter()
            .nth(index)
            .ok_or(BackendError::NotFound)
    }
}

/// The addresses to show as "Server IP Addresses": the listen address, or,
/// when the server listens on every address, the one the machine's routing
/// table picks for reaching other networks. A UDP socket's `connect` sends
/// nothing; it only asks the operating system for that route (inf).
fn reachable_addresses(listen: SocketAddr) -> Vec<String> {
    if !listen.ip().is_unspecified() {
        return vec![listen.ip().to_string()];
    }
    let probe: SocketAddr = match listen.ip() {
        IpAddr::V4(_) => ([192, 0, 2, 1], 9).into(),
        IpAddr::V6(_) => ("2001:db8::1".parse::<IpAddr>().expect("an address"), 9).into(),
    };
    let unspecified: SocketAddr = (listen.ip(), 0).into();
    UdpSocket::bind(unspecified)
        .and_then(|socket| {
            socket.connect(probe)?;
            socket.local_addr()
        })
        .ok()
        .map(|local| local.ip())
        .filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
        .map(|ip| vec![ip.to_string()])
        .unwrap_or_default()
}

/// The same FileMaker Server state under the Admin API's name
/// (`FileStatus::filemaker_name`, `DatabaseStatus::api_name`).
fn status(status: FileStatus) -> DatabaseStatus {
    match status {
        FileStatus::Open => DatabaseStatus::Normal,
        FileStatus::Closing => DatabaseStatus::Closing,
        FileStatus::Closed => DatabaseStatus::Closed,
        FileStatus::Opening => DatabaseStatus::Opening,
        FileStatus::Paused => DatabaseStatus::Paused,
    }
}

/// What a backup schedule backs up, as the console words it.
fn backup_target(files: &BackupFiles) -> String {
    match files {
        BackupFiles::All => "All Databases".into(),
        BackupFiles::Folder(folder) => folder.display().to_string(),
        BackupFiles::File(name) => name.clone(),
    }
}

fn system_time(zoned: &jiff::Zoned) -> SystemTime {
    SystemTime::from(zoned.timestamp())
}

impl Backend for ServerBackend {
    fn alerts(&self) -> Vec<Alert> {
        Vec::new()
    }

    fn acknowledge_alert(&self, _id: u64) -> bool {
        false
    }

    fn server(&self) -> ServerInfo {
        let overview = self.handle.overview();
        ServerInfo {
            name: overview.name,
            id: overview.server_id,
            version: overview.version,
            started_at: overview.started_at,
            address: overview.address,
            ip_addresses: self.ip_addresses.clone(),
        }
    }

    fn databases(&self) -> Vec<Database> {
        self.handle
            .files()
            .into_iter()
            .map(|file| Database {
                id: file.id,
                name: file.name,
                folder: file.folder,
                size: file.size,
                status: status(file.status),
                clients: file.clients,
                encrypted: false,
            })
            .collect()
    }

    fn clients(&self) -> Vec<Client> {
        self.handle
            .clients()
            .into_iter()
            .map(|client| Client {
                id: client.id,
                session_name: client.session_name,
                machine_name: client.machine_name,
                address: client.address,
                app_type: client.app_type,
                version: client.version,
                connected_at: client.connected_at,
                files: client
                    .files
                    .into_iter()
                    .map(|file| ClientFile {
                        id: file.id,
                        name: file.name,
                        account: file.account,
                    })
                    .collect(),
                disconnecting: client.disconnecting,
            })
            .collect()
    }

    fn disconnect_client(&self, id: u32, notice: Option<&Notice>) -> Result<(), BackendError> {
        if notice.is_some() {
            // Asking a client to close its files first is not built yet:
            // OpenMaker Server's Disconnect is immediate (docs/admin-console.md,
            // Limits).
            return Err(BackendError::Unsupported);
        }
        if self.handle.disconnect_client(id) {
            Ok(())
        } else {
            Err(BackendError::NotFound)
        }
    }

    /// The database folders the server looked in at startup, empty or
    /// not, and the folders of files named on its command line elsewhere.
    fn folders(&self) -> Result<Folders, BackendError> {
        let databases: BTreeSet<PathBuf> = self
            .handle
            .database_folders()
            .into_iter()
            .chain(self.handle.files().into_iter().map(|file| file.folder))
            .collect();
        Ok(Folders {
            databases: databases.into_iter().collect(),
            backups: None,
        })
    }

    fn certificate(&self) -> Result<Vec<Vec<u8>>, BackendError> {
        Ok(self.certificates.clone())
    }

    fn schedules(&self) -> Result<Vec<Schedule>, BackendError> {
        Ok(self
            .handle
            .schedules()
            .into_iter()
            .zip(FIRST_SCHEDULE_ID..)
            .map(|(status, id)| {
                let (kind, target, account, what) = match status.schedule.task {
                    ScheduleTask::Script(task) => (
                        ScheduleKind::FileMakerScript,
                        Some(task.file),
                        Some(task.account),
                        "script",
                    ),
                    ScheduleTask::Backup(task) => (
                        ScheduleKind::Backup,
                        Some(backup_target(&task.files)),
                        None,
                        "backup",
                    ),
                };
                Schedule {
                    id,
                    name: status.schedule.name,
                    kind,
                    target,
                    account,
                    enabled: status.schedule.enabled,
                    next_run: status.next_run.as_ref().map(system_time),
                    last_run: status.last_run.as_ref().map(|run| ScheduleRun {
                        started_at: system_time(&run.started),
                        succeeded: run.succeeded(),
                        detail: (!run.succeeded()).then(|| {
                            if run.ran {
                                format!("The {what} ended with error {}.", run.error)
                            } else {
                                format!("The {what} did not run (error {}).", run.error)
                            }
                        }),
                    }),
                    running: status.running_since.is_some(),
                }
            })
            .collect())
    }

    fn run_schedule(&self, id: u32) -> Result<(), BackendError> {
        let status = self.schedule_by_id(id)?;
        self.handle
            .run_schedule(&status.schedule.name)
            .map_err(|e| BackendError::InvalidState(e.to_string()))
    }

    fn enable_schedule(&self, id: u32, enabled: bool) -> Result<(), BackendError> {
        let status = self.schedule_by_id(id)?;
        self.handle
            .enable_schedule(&status.schedule.name, enabled)
            .map_err(|e| BackendError::Failed(e.to_string()))
    }

    fn data_folder(&self) -> Option<PathBuf> {
        self.handle
            .files()
            .into_iter()
            .next()
            .map(|file| file.folder)
    }
}
