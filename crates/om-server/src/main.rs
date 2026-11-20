//! `om-server`: host `.fmp12` files for FileMaker-protocol clients.
//!
//! Without `--cert`/`--key` it uses the operating system's own self-signed
//! certificate, or one it makes at its first start and keeps
//! (`om_server::default_identity`), as a FileMaker Server install keeps its
//! default certificate, and logs which with its fingerprint. Clients log in
//! with each hosted file's own accounts and passwords, and a file FileMaker
//! Server would not host for its open full access is refused unless
//! `--allow-insecure-files`. Log verbosity follows `RUST_LOG` (default `info`); the event
//! log in FileMaker Server's format goes to `--log-folder`, by default
//! `Logs` in the state folder. On Unix it raises its open-file limit as far
//! as its connection cap needs and the hard limit allows, and SIGTERM or
//! SIGINT stops it as an administrator's stop does. Backup schedules
//! without a target of their own write to `--backup-folder`, by default
//! `Backups` in the state folder.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use om_server::default_identity::{self, Source, SYSTEM_CERTIFICATES};
use om_server::{
    parse_schedules, CalcLimits, ConnectionLimits, HostedFile, Identity, ScheduleTask,
    ScriptLimits, Server, ServerConfig, ServerIdentity, SCRIPT_CALC_LIMITS,
};

const USAGE: &str = "usage: om-server [--bind <addr:port>] [--name <server name>]
                 [--version-string <text>] [--cert <pem> --key <pem>]
                 [--state-dir <dir>] [--cert-name <name>]...
                 [--log-folder <dir>] [--allow-insecure-files]
                 [--admin-user <name> [--admin-password-file <file>]
                  [--admin-bind <addr:port>]]
                 [--record <dir>]
                 [--scripts <n>] [--script-steps <n>] [--script-seconds <n>]
                 [--calc-text-mib <n>]
                 [--connections <n>] [--connections-per-address <n>]
                 [--upload-memory-mib <n>] [--download-memory-mib <n>]
                 [--idle-seconds <n>|off]
                 [--schedules <file>] [--backup-folder <dir>]
                 [--list <file name>]... [--list-all]
                 [<file.fmp12>...]

  --bind            listen address (default 0.0.0.0:5003, FileMaker's port)
  --name            server name clients see (default \"openmaker\")
  --version-string  version clients see (default \"OpenMaker Server <version>\")
  --cert, --key     PEM certificate chain and key. Without them the server
                    uses the operating system's own self-signed certificate
                    when it may read its key (Debian and Ubuntu: ssl-cert's
                    snakeoil), or else one it makes at its first start and
                    keeps as <state dir>/tls/cert.pem, the file OpenMaker
                    clients are given in OPENMAKER_CA_CERT to trust it. The
                    certificate used and its SHA-256 fingerprint are logged
  --state-dir       folder for the certificate the server makes and, without
                    --backup-folder, its backups (default $STATE_DIRECTORY,
                    set by systemd's StateDirectory=, else the user's state
                    folder: $XDG_STATE_HOME/openmaker-server or
                    ~/.local/state/openmaker-server on Linux)
  --log-folder      folder of the event log, FileMaker Server's Event.log
                    (the server, its files, its schedules) and Access.log
                    (clients), each renamed to its -old file at 40 MiB
                    (default <state dir>/Logs, made for the server's user
                    only). Without a state folder and without it, no event
                    log is written
  --cert-name       a host name or IP address clients connect by, named in
                    the certificate the server makes besides localhost, the
                    --bind address and the --name. A kept certificate keeps
                    the names it was made with: remove its folder to have
                    one made with new names
  --allow-insecure-files
                    host files FileMaker Server refuses by default (its
                    securefilesonly=false): a [Full Access] account with no
                    password, as new files and the starter solutions have,
                    or with a password the file stores for its \"Log in
                    using\" option, or [Guest] with [Full Access]. Anyone who
                    reaches the port can then open such a file with full
                    access. Clients always log in with each file's own
                    accounts and passwords
  --admin-user      run the Admin Console, a web page for administering the
                    server (https://<--admin-bind>/admin-console), with this
                    account. Its password, which may not be empty, comes from
                    --admin-password-file, or else from the OM_ADMIN_PASSWORD
                    environment variable. The console presents the server's
                    TLS certificate
  --admin-password-file
                    file holding the --admin-user password, on one line; other
                    users may not read it (mode 0600, or 0640 for a group).
                    Under systemd, LoadCredential=<name>:<file> in the unit
                    and --admin-password-file %d/<name>. Unlike
                    OM_ADMIN_PASSWORD, which the server removes from its
                    environment but which stays readable in the environment
                    it started with (/proc/<pid>/environ), the password never
                    enters the environment
  --admin-bind      the console's address (default 127.0.0.1:16000: this
                    machine only; 0.0.0.0:16000 to administer from others)
  --scripts         Perform Script on Server scripts executing at once
                    (default 100; more are refused with error 812)
  --script-steps    steps one script may run before it is aborted
                    (default 100000000)
  --script-seconds  seconds one script may run before it is aborted, even
                    inside a calculation (default 1800)
  --calc-text-mib   MiB one text value a script's calculation builds may
                    take; past it the calculation ends with error 7
                    (default 256; FileMaker documents 2 GB for text)
  --connections     connections served at once, TLS handshakes included
                    (default 4096); one more is closed as soon as it is
                    accepted. A client holds one for each file it has open
  --connections-per-address
                    of those, from one client address, an IPv6 /64 counting
                    as one (default 1024: an office behind one NAT address
                    shares it)
  --upload-memory-mib
                    memory held for clients' uploads, as bytes and as trees,
                    on the whole server: uploads arriving in parts, records
                    staged and not committed, and an upload while it is read
                    (default 4096). One client address may hold a quarter (an
                    office behind one NAT address shares it); past it a
                    request fails as over a limit. At the default one address
                    can upload a container of about 465 MiB: raise it for
                    larger ones. It is not safe on a machine with much less
                    than 8 GB of memory: set it lower there
  --download-memory-mib
                    memory held for downloads being paged to clients, on the
                    whole server (default 2048). Past it the downloads left
                    unfinished longest, on any connection, are dropped; a
                    client that continues one gets an error
  --idle-seconds    close a connection with a session that sends nothing for
                    this long (default 3600; off: never). A client with a file
                    open sends a keepalive every 60 s. A connection without a
                    session is closed after 145 s, or this if shorter
  --schedules       schedules to run, one [name] section each: type (script,
                    the default, or backup); repeat (once, daily, every <n>
                    days, weekly <days>), at <time> or every <n>
                    minutes|hours from <time> to <time>, start and end
                    dates, enabled. A script schedule: file, script,
                    account (one of the file's that may log in to it; no
                    password), parameter, time limit, end on
                    time limit. A backup schedule: back up (all, folder
                    <folder> or file <name>), keep (1 to 99 backups that are
                    not preserved; default 1), target (default the
                    --backup-folder). See om-server's schedules/file.rs
  --backup-folder   default folder for backups (default Backups in the state
                    folder, see --state-dir). Each backup is a folder
                    <schedule>_YYYY-MM-DD_HHMM holding backup.config and
                    Databases/, as FileMaker Server writes them; files
                    unchanged since the schedule's last backup are hard
                    links to its copies. It holds whole databases: the
                    server makes it owner-only when it makes it
  --list            name a hosted file (as clients open it, e.g. Tasks.fmp12)
                    in the file list OpenMaker clients' Hosts dialog shows.
                    Files are unlisted by default: a file's own \"Don't
                    display in Open Remote File dialog\" setting is not read
                    yet. Unlisted files still open by name; listed ones still
                    need an account. Anyone who can reach the port sees the
                    listed names (not their paths)
  --list-all        list every hosted file
  --record          save every connection's GIOP messages under <dir>/conn-NNN,
                    numbered after the highest conn-NNN already there
                    (holds client license keys; keep it out of git)";

struct Args {
    bind: String,
    name: String,
    version_string: String,
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    log_folder: Option<PathBuf>,
    cert_names: Vec<String>,
    insecure_files: bool,
    admin_user: Option<String>,
    admin_password_file: Option<PathBuf>,
    admin_bind: Option<String>,
    record: Option<PathBuf>,
    limits: ScriptLimits,
    connections: ConnectionLimits,
    calc_limits: CalcLimits,
    schedules: Option<PathBuf>,
    backup_folder: Option<PathBuf>,
    listed: Vec<String>,
    list_all: bool,
    files: Vec<PathBuf>,
}

fn number<T: std::str::FromStr>(flag: &str, value: String) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} needs a whole number, not {value:?}"))
}

/// [`number`], above 0.
fn positive<T: std::str::FromStr + Default + PartialEq>(
    flag: &str,
    value: String,
) -> Result<T, String> {
    let n = number(flag, value)?;
    if n == T::default() {
        return Err(format!("{flag} needs a number above 0"));
    }
    Ok(n)
}

fn host_name(flag: &str, value: String) -> Result<String, String> {
    if default_identity::is_host_name(&value) {
        Ok(value)
    } else {
        Err(format!(
            "{flag} needs a host name or IP address, not {value:?}"
        ))
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        bind: "0.0.0.0:5003".into(),
        name: "openmaker".into(),
        version_string: format!("OpenMaker Server {}", env!("CARGO_PKG_VERSION")),
        cert: None,
        key: None,
        state_dir: None,
        log_folder: None,
        cert_names: Vec::new(),
        insecure_files: false,
        admin_user: None,
        admin_password_file: None,
        admin_bind: None,
        record: None,
        limits: ScriptLimits::DEFAULT,
        connections: ConnectionLimits::DEFAULT,
        calc_limits: SCRIPT_CALC_LIMITS,
        schedules: None,
        backup_folder: None,
        listed: Vec::new(),
        list_all: false,
        files: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--bind" => args.bind = value()?,
            "--name" => args.name = value()?,
            "--version-string" => args.version_string = value()?,
            "--cert" => args.cert = Some(value()?.into()),
            "--key" => args.key = Some(value()?.into()),
            "--state-dir" => args.state_dir = Some(value()?.into()),
            "--log-folder" => args.log_folder = Some(value()?.into()),
            "--cert-name" => args.cert_names.push(host_name(&arg, value()?)?),
            "--allow-insecure-files" => args.insecure_files = true,
            "--admin-user" => args.admin_user = Some(value()?),
            "--admin-password-file" => args.admin_password_file = Some(value()?.into()),
            "--admin-bind" => args.admin_bind = Some(value()?),
            "--record" => args.record = Some(value()?.into()),
            "--schedules" => args.schedules = Some(value()?.into()),
            "--backup-folder" => args.backup_folder = Some(value()?.into()),
            "--list" => args.listed.push(value()?),
            "--list-all" => args.list_all = true,
            "--scripts" => args.limits.concurrent = number(&arg, value()?)?,
            "--script-steps" => args.limits.steps = number(&arg, value()?)?,
            "--script-seconds" => {
                args.limits.duration = Duration::from_secs(number(&arg, value()?)?)
            }
            "--calc-text-mib" => {
                let mib: usize = positive(&arg, value()?)?;
                args.calc_limits.text = mib.saturating_mul(1 << 20)
            }
            "--connections" => args.connections.connections = positive(&arg, value()?)?,
            "--connections-per-address" => args.connections.per_address = positive(&arg, value()?)?,
            "--upload-memory-mib" => {
                let mib: usize = positive(&arg, value()?)?;
                args.connections.upload_memory = mib.saturating_mul(1 << 20)
            }
            "--download-memory-mib" => {
                let mib: usize = positive(&arg, value()?)?;
                args.connections.download_memory = mib.saturating_mul(1 << 20)
            }
            "--idle-seconds" => {
                args.connections.idle = match value()?.as_str() {
                    "off" => None,
                    seconds => Some(Duration::from_secs(positive(&arg, seconds.into())?)),
                }
            }
            "-h" | "--help" => return Err(String::new()),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            file => args.files.push(file.into()),
        }
    }
    if args.admin_user.is_none() {
        for (given, flag) in [
            (args.admin_password_file.is_some(), "--admin-password-file"),
            (args.admin_bind.is_some(), "--admin-bind"),
        ] {
            if given {
                return Err(format!("{flag} needs --admin-user"));
            }
        }
    }
    if args.admin_user.as_deref() == Some("") {
        return Err("--admin-user needs a name".into());
    }
    if args.admin_user.is_some() && !cfg!(feature = "admin-console") {
        return Err("--admin-user: this om-server was built without the Admin Console".into());
    }
    for name in &args.listed {
        let hosted = args
            .files
            .iter()
            .any(|path| file_name(path).to_lowercase() == name.to_lowercase());
        if !hosted {
            return Err(format!("--list {name:?}: no hosted file has that name"));
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    use_pdf_worker();
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("error: {e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(64);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    raise_open_file_limit(&args);
    if args.insecure_files {
        log::warn!(
            "--allow-insecure-files: a file whose [Full Access] needs no password opens with \
             full access to anyone who can reach the server"
        );
    }
    // Read before the server starts any thread (it may change the
    // environment).
    let admin = admin_console_account(&args)?;
    let identity = tls_identity(&args)?;
    #[cfg(feature = "admin-console")]
    let console_identity = admin.as_ref().map(|_| identity.clone());
    // No files is a server with nothing hosted yet, as FileMaker Server
    // starts before any database is opened: its Admin Console and listener
    // still serve, with an empty file list (#419).
    if args.files.is_empty() {
        log::info!("hosting no files");
    }
    let files = args
        .files
        .iter()
        .map(|path| HostedFile {
            name: file_name(path),
            path: path.clone(),
        })
        .collect();
    let config = ServerConfig {
        identity: ServerIdentity {
            server_id: server_id(&args.name),
            name: args.name,
            version_string: args.version_string,
            web_service_settings: String::new(),
        },
        files,
        insecure_files: args.insecure_files,
        record_dir: args.record,
    };
    let schedules = match &args.schedules {
        Some(path) => {
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            parse_schedules(&text).map_err(|e| format!("{}: {e}", path.display()))?
        }
        None => Vec::new(),
    };
    let backup_folder = args.backup_folder.clone().or_else(|| {
        args.state_dir
            .clone()
            .or_else(default_identity::state_folder)
            .map(|state| state.join("Backups"))
    });
    let backups = schedules
        .iter()
        .filter(|s| matches!(&s.task, ScheduleTask::Backup(task) if task.folder.is_none()))
        .count();
    let log_folder = args.log_folder.clone().or_else(|| {
        let state = args
            .state_dir
            .clone()
            .or_else(default_identity::state_folder)?;
        Some(state.join("Logs"))
    });
    let mut server = Server::bind(&args.bind, config, identity)?;
    match log_folder {
        Some(folder) => server = server.with_event_log(folder)?,
        None => log::warn!("no state folder and no --log-folder: no event log is written"),
    }
    let mut server = server
        .with_script_limits(args.limits)
        .with_connection_limits(args.connections)
        .with_calc_limits(args.calc_limits)
        .with_schedules(schedules)?;
    match backup_folder {
        Some(folder) => {
            if backups > 0 {
                log::info!("backups go to {}", folder.display());
            }
            server = server.with_backup_folder(folder)?;
        }
        None if backups > 0 => log::warn!(
            "no --backup-folder and no state folder to put backups in: the {backups} backup \
             schedule(s) without a target will fail"
        ),
        None => {}
    }
    if args.list_all {
        server = server.listing_all_files();
    }
    for name in &args.listed {
        server = server.listing_file(name);
    }
    let handle = server.handle()?;
    #[cfg(unix)]
    let stopped = stop_on_signals(handle.clone())?;
    #[cfg(feature = "admin-console")]
    let _console = match (admin, console_identity) {
        (Some((user, password, bind)), Some(identity)) => Some(
            om_server::console::start_console(server.handle()?, &identity, bind, user, password)
                .map_err(|e| format!("the Admin Console on {bind}: {e}"))?,
        ),
        _ => None,
    };
    #[cfg(not(feature = "admin-console"))]
    let _ = admin;
    server.serve()?;
    // The accept loop ended: a signal's stop is under way; let it finish
    // (its scripts' grace).
    #[cfg(unix)]
    let _ = stopped.recv_timeout(STOP_GRACE + Duration::from_secs(5));
    // End the clients' sessions, so that the event log records them, then
    // write its last lines.
    handle.close_connections();
    let deadline = std::time::Instant::now() + SESSIONS_GRACE;
    while !handle.clients().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if !handle.close_event_log(LOG_CLOSE) {
        log::warn!("the event log was not written to its end in {LOG_CLOSE:?}");
    }
    Ok(())
}

/// How long a stopped server waits for its clients' sessions to end once
/// their connections are closed, and for its event log's last lines
/// (OpenMaker's choice).
const SESSIONS_GRACE: Duration = Duration::from_secs(5);
const LOG_CLOSE: Duration = Duration::from_secs(5);

/// How long a server stopped by a signal waits for its scripts to revert
/// (OpenMaker's choice).
#[cfg(unix)]
const STOP_GRACE: Duration = Duration::from_secs(10);

/// Stop the server at the first SIGTERM or SIGINT as an administrator's
/// stop does (`ServerHandle::shutdown`, which the event log records), on a
/// thread of its own, and end the process at once at a second, also while
/// the stop runs. The receiver is told when the stop is done.
#[cfg(unix)]
fn stop_on_signals(
    handle: om_server::ServerHandle,
) -> std::io::Result<std::sync::mpsc::Receiver<()>> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM])?;
    let (stopped, done) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("om-server signals".into())
        .spawn(move || {
            let mut signals = signals.forever();
            if let Some(signal) = signals.next() {
                log::info!("stopping: signal {signal}");
                let stop = std::thread::Builder::new()
                    .name("om-server stop".into())
                    .spawn(move || {
                        if !handle.shutdown(STOP_GRACE) {
                            log::warn!(
                                "scripts were still executing {STOP_GRACE:?} after the stop"
                            );
                        }
                        let _ = stopped.send(());
                    });
                if let Err(e) = stop {
                    log::error!("cannot stop: {e}; ending at once");
                    std::process::exit(1);
                }
            }
            if let Some(signal) = signals.next() {
                log::warn!("signal {signal} again: ending at once");
                std::process::exit(1);
            }
        })?;
    Ok(done)
}

/// The Admin Console's account and address, when `--admin-user` asks for
/// it: the user name, its password's digest and where the console listens.
#[cfg_attr(not(feature = "admin-console"), allow(unused_variables))]
fn admin_console_account(
    args: &Args,
) -> Result<Option<(String, AdminPassword, std::net::SocketAddr)>, String> {
    let Some(user) = args.admin_user.clone() else {
        return Ok(None);
    };
    let bind = args.admin_bind.as_deref().unwrap_or(ADMIN_BIND);
    let bind = bind
        .parse()
        .map_err(|_| format!("--admin-bind {bind:?}: not an address and port"))?;
    let (password, source) = password(
        args.admin_password_file.as_deref(),
        ADMIN_PASSWORD_VARIABLE,
        "--admin-user",
        "--admin-password-file",
    )?;
    if password.is_empty() {
        return Err(format!(
            "{source} is empty: the Admin Console account needs a password"
        ));
    }
    Ok(Some((user, password.as_str().into(), bind)))
}

#[cfg(feature = "admin-console")]
type AdminPassword = om_admin::PasswordDigest;
#[cfg(not(feature = "admin-console"))]
type AdminPassword = String;

#[cfg(feature = "admin-console")]
const ADMIN_BIND: &str = om_server::console::DEFAULT_ADMIN_BIND;
#[cfg(not(feature = "admin-console"))]
const ADMIN_BIND: &str = "127.0.0.1:16000";

/// The environment variable that may hold the `--admin-user` password.
const ADMIN_PASSWORD_VARIABLE: &str = "OM_ADMIN_PASSWORD";

/// Descriptors kept for the server's own use beside its connections:
/// hosted files and their locks, saves' new copies, container files.
#[cfg(unix)]
const RESERVED_DESCRIPTORS: u64 = 512;

/// Raise the soft open-file limit to what `--connections` needs, as far
/// as the hard limit allows. Each connection holds two descriptors (its
/// socket, and a handle to close it with), four while recording; a
/// service's soft limit is often 1024 (systemd's default), far below what
/// the default cap needs. The server serves fewer connections when the
/// limit leaves room for fewer (`Server::serve` warns), so that its files
/// can still be saved.
#[cfg(unix)]
fn raise_open_file_limit(args: &Args) {
    use rustix::process::{getrlimit, setrlimit, Resource, Rlimit};
    let per_connection = if args.record.is_some() { 4 } else { 2 };
    let connections = u64::try_from(args.connections.connections).unwrap_or(u64::MAX);
    let wanted = connections
        .saturating_mul(per_connection)
        .saturating_add(RESERVED_DESCRIPTORS);
    let limit = getrlimit(Resource::Nofile);
    // `None` is unlimited.
    let current = limit.current.unwrap_or(u64::MAX);
    if current >= wanted {
        return;
    }
    let raised = limit.maximum.map_or(wanted, |maximum| maximum.min(wanted));
    if raised > current {
        let new = Rlimit {
            current: Some(raised),
            maximum: limit.maximum,
        };
        match setrlimit(Resource::Nofile, new) {
            Ok(()) => log::info!("raised the open-file limit from {current} to {raised}"),
            Err(e) => log::warn!("cannot raise the open-file limit from {current}: {e}"),
        }
    }
}

/// The longest password file read.
const MAX_PASSWORD_FILE: u64 = 64 * 1024;

/// The password `flag`'s account signs in with, and where it came from: the
/// file `file_flag` names, or else the environment variable `variable`,
/// which is then removed from the environment so that nothing the server
/// starts inherits it and nothing reads it later. The environment the
/// process started with is not changed by that and stays readable in
/// `/proc/<pid>/environ`, so the variable is logged as the weaker way.
/// Called before the server starts any thread.
fn password(
    file: Option<&Path>,
    variable: &str,
    flag: &str,
    file_flag: &str,
) -> Result<(String, String), String> {
    match (file, std::env::var_os(variable)) {
        (Some(_), Some(_)) => Err(format!(
            "both {variable} and {file_flag} give the password: unset {variable}"
        )),
        (Some(path), None) => Ok((
            read_password_file(path)?,
            format!("the password in {}", path.display()),
        )),
        (None, Some(value)) => {
            std::env::remove_var(variable);
            let password = value
                .into_string()
                .map_err(|_| format!("{variable} is not valid Unicode"))?;
            log::warn!(
                "the {flag} password comes from {variable}, which stays readable in the \
                 environment the server started with (/proc/<pid>/environ): {file_flag} \
                 keeps it out of the environment"
            );
            Ok((password, variable.into()))
        }
        (None, None) => Err(format!(
            "{flag} needs its password: {file_flag} <file>, or {variable}"
        )),
    }
}

/// The password in the file `path`: its text without one final line ending.
/// Refused when users other than the owner and its group may read it, or
/// the group may change it, and when it is larger than a password, not
/// UTF-8, or more than one line. Nothing of the text is put in an error.
fn read_password_file(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let failed = |problem: &dyn std::fmt::Display| format!("{}: {problem}", path.display());
    let file = std::fs::File::open(path).map_err(|e| failed(&e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = file.metadata().map_err(|e| failed(&e))?.mode() & 0o777;
        if mode & 0o027 != 0 {
            return Err(failed(&format_args!(
                "other users may read or change it (mode {mode:04o}): make it 0600, or 0640 \
                 for a group the server's user is in"
            )));
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_PASSWORD_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| failed(&e))?;
    if bytes.len() as u64 > MAX_PASSWORD_FILE {
        return Err(failed(&"larger than a password"));
    }
    let mut text = String::from_utf8(bytes).map_err(|_| failed(&"not UTF-8 text"))?;
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    if text.contains(['\n', '\r']) {
        return Err(failed(&"holds more than one line"));
    }
    Ok(text)
}

/// The server's TLS certificate: `--cert`/`--key`, or else the operating
/// system's or the kept one. Logged with where it came from and its
/// fingerprint, which an administrator compares with what clients show.
fn tls_identity(args: &Args) -> Result<Identity, Box<dyn std::error::Error>> {
    let read = |path: &Path| std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
    let (identity, source) = match (&args.cert, &args.key) {
        (Some(cert), Some(key)) => (
            Identity::from_pem(&read(cert)?, &read(key)?)?,
            Source::Given(cert.clone()),
        ),
        (None, None) => {
            let state = args
                .state_dir
                .clone()
                .or_else(default_identity::state_folder);
            default_identity::choose(
                SYSTEM_CERTIFICATES,
                state.as_deref(),
                &certificate_names(args),
            )?
        }
        _ => return Err("--cert and --key go together".into()),
    };
    log::info!(
        "TLS certificate: {source}; SHA-256 fingerprint {}",
        identity.fingerprint()
    );
    for name in args.cert_names.iter().filter(|name| !identity.names(name)) {
        let remedy = match &source {
            Source::Made(folder) | Source::Kept(folder) => format!(
                "it keeps the names it was made with; remove {} to have one made with \
                 the names given now",
                folder.display()
            ),
            Source::Given(_) | Source::System(_) => "give one that names it".into(),
        };
        log::warn!(
            "the TLS certificate does not name {name}, so OpenMaker clients connecting by \
             that name will refuse it: {remedy}"
        );
    }
    Ok(identity)
}

/// The names a certificate the server makes is for: loopback, the `--bind`
/// address unless it is every address, the `--name` when it is a host
/// name, and each `--cert-name`.
fn certificate_names(args: &Args) -> Vec<String> {
    let bind = args
        .bind
        .rsplit_once(':')
        .map_or(args.bind.as_str(), |(host, _)| host)
        .trim_start_matches('[')
        .trim_end_matches(']');
    let every = bind
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_unspecified());
    let mut names: Vec<String> = Vec::new();
    let candidates = ["localhost", "127.0.0.1", "::1", args.name.as_str()]
        .into_iter()
        .chain((!every).then_some(bind))
        .chain(args.cert_names.iter().map(String::as_str));
    for name in candidates {
        if default_identity::is_host_name(name)
            && !names.iter().any(|n| n.eq_ignore_ascii_case(name))
        {
            names.push(name.to_string());
        }
    }
    names
}

/// The name a file is opened by: its file name, with `.fmp12` added if the
/// path has no extension (FileMaker Server's sample files have none).
fn file_name(path: &Path) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    if path.extension().is_some() {
        name
    } else {
        format!("{name}.fmp12")
    }
}

/// A stable 32-hex server id derived from the server name, so it survives
/// restarts the way FileMaker Server's persistent id does.
fn server_id(name: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, name.as_bytes());
    digest.as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::read_password_file;

    #[test]
    fn password_files_are_private_and_one_line() {
        let folder = std::env::temp_dir().join(format!("om-password-file-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = |name: &str, text: &str, mode: u32| {
            let path = folder.join(name);
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        // One final line ending is not part of the password; spaces are.
        let read = |path: std::path::PathBuf| read_password_file(&path);
        assert_eq!(
            read(file("unix", " pass word \n", 0o600)).unwrap(),
            " pass word "
        );
        assert_eq!(read(file("windows", "pass\r\n", 0o640)).unwrap(), "pass");
        assert_eq!(read(file("bare", "pass", 0o400)).unwrap(), "pass");
        for (name, text, mode, problem) in [
            (
                "others",
                "s3cret\n",
                0o644,
                "other users may read or change it (mode 0644)",
            ),
            ("group-write", "s3cret\n", 0o660, "(mode 0660)"),
            ("lines", "s3cret\nn3xt\n", 0o600, "more than one line"),
        ] {
            let error = read(file(name, text, mode)).unwrap_err();
            assert!(error.contains(problem), "{name}: {error}");
            // Nothing of the file's text is shown.
            assert!(
                !error.contains("s3cret") && !error.contains("n3xt"),
                "{name}"
            );
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }
}

/// PDF data in container fields is read by `om-server-pdf-worker` beside
/// this program (#566); without it, calculations reading PDF data fail with
/// error 3.
fn use_pdf_worker() {
    use om_calc::functions::pdf_worker;
    let Some(worker) = pdf_worker::sibling("om-server-pdf-worker") else {
        return;
    };
    if !worker.is_file() && std::env::var_os(pdf_worker::WORKER_VARIABLE).is_none() {
        log::warn!(
            "{} is missing, so calculations will not read PDF data in containers \
             (`cargo build -p om-server` builds it)",
            worker.display()
        );
    }
    pdf_worker::set_default(worker);
}
