//! What a server hosts and how it identifies itself.

use std::path::{Path, PathBuf};
use std::time::Duration;

use om_calc::eval::CalcLimits;

/// How the server describes itself in the `Connect` reply.
#[derive(Clone, Debug)]
pub struct ServerIdentity {
    /// The server's canonical name (`remoteName`), also the Open reply's
    /// `serverMsg`. FileMaker Server uses its short host name.
    pub name: String,
    /// The version string clients display, e.g. `"OpenMaker Server 0.0"`.
    pub version_string: String,
    /// The server's 32-hex id (`installCode` slot of the reply).
    pub server_id: String,
    /// The `port=...;ssl=...` web-services settings; empty when the server
    /// offers no HTTP endpoints.
    pub web_service_settings: String,
}

/// One hosted `.fmp12` file.
#[derive(Clone, Debug)]
pub struct HostedFile {
    /// The name clients open it by, e.g. `test project.fmp12`. Matched
    /// case-insensitively, as FileMaker does.
    pub name: String,
    pub path: PathBuf,
}

/// What an account may do in the files, as far as the server knows: the
/// privileges a Perform Script on Server session runs with
/// (script-runtime.md 9, 11.3).
///
/// An account of a file in one of its verified built-in sets
/// (`hosting::FileAccount::privileges`, session-identity.md 2.7) has that
/// set's privileges. Every other account is [`Privileges::Unknown`] and
/// fails closed in the server's own sessions: performed at the top or by
/// an unelevated script, it runs the scripts its custom set makes
/// executable (the session's set decides, `ServerHost::set_access`; a
/// script running with full access may perform one without access too),
/// and steps that create, change, delete, find or read records without
/// the elevation of a script set to run with full access privileges fail
/// with FileMaker's error 9 (insufficient privileges).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Privileges {
    /// The [Full Access] privilege set.
    FullAccess,
    /// [Data Entry Only]: every record viewed, created, edited and
    /// deleted, every script executable, no design.
    DataEntry,
    /// [Read-Only Access]: every record viewed only, every script
    /// executable, no design.
    ReadOnly,
    /// Any other privilege set, a custom set's: its data limits (records,
    /// fields, layouts) are not applied to server scripts, so a step reads
    /// and writes nothing unless elevated; its script entries are, through
    /// the session's set (`ServerHost::set_access`).
    #[default]
    Unknown,
}

impl Privileges {
    /// Whether the account may run every script of the file: a verified
    /// built-in set's account (scripts modifiable or executable only).
    pub fn runs_scripts(self) -> bool {
        self != Self::Unknown
    }

    /// Whether the account may read every record: a verified built-in
    /// set's account.
    pub fn views_records(self) -> bool {
        self != Self::Unknown
    }

    /// Whether the account may create, edit and delete every record.
    pub fn edits_records(self) -> bool {
        matches!(self, Self::FullAccess | Self::DataEntry)
    }
}

/// Limits on Perform Script on Server scripts. FileMaker has no step or time
/// limit (script-runtime.md 2.6); these are OpenMaker Server's, so that a
/// script that never ends cannot hold its record locks, transaction and
/// script slot forever. A script past a limit is aborted as an
/// administrator's abort is: its open transaction and uncommitted record
/// are reverted and its locks released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptLimits {
    /// Scripts that may execute at once; one more is refused with 812
    /// (`sLimitOfScriptsExecuting`).
    pub concurrent: usize,
    /// Steps one script may execute.
    pub steps: u64,
    /// How long one script may run; a calculation still running then
    /// stops too (`om_calc::eval::CalcHost::interrupted`).
    pub duration: Duration,
}

impl ScriptLimits {
    /// 100 scripts at once, each at most 100 million steps and 30 minutes.
    pub const DEFAULT: ScriptLimits = ScriptLimits {
        concurrent: 100,
        steps: 100_000_000,
        duration: Duration::from_secs(30 * 60),
    };
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How many connections the server serves at once, and how long it keeps
/// one whose client does not do its part. Anyone who can reach the port
/// can open connections (a client certificate of its own making completes
/// TLS, and `Connect` needs no account), and each holds a thread, so these
/// bound what that costs. FileMaker Server's own limits are not measured;
/// the defaults are OpenMaker's choices ([`ConnectionLimits::DEFAULT`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionLimits {
    /// Connections served at once, counted from their accept (before TLS)
    /// to their end. One more is closed as soon as it is accepted. Fewer
    /// when the open-file limit leaves room for fewer: each connection
    /// holds two descriptors (four while recording), and some are kept for
    /// the server's own work, such as saving its files.
    pub connections: usize,
    /// Connections served at once from one client address; an IPv6 /64
    /// counts as one address. One more from it is closed as soon as it is
    /// accepted.
    pub per_address: usize,
    /// How long a connection's whole TLS handshake may take.
    pub handshake: Duration,
    /// Close a connection with a session (`Connect`) that receives nothing
    /// for this long after answering its last request; `None`: never. The
    /// connection gets a GIOP CloseConnection and its session ends
    /// (network-protocol.md 6.2).
    pub idle: Option<Duration>,
    /// The same for a connection without a session (before `Connect`, or
    /// after `Disconnect`), or `idle` if that is shorter.
    pub idle_without_session: Duration,
    /// End a connection whose client takes none of what the server writes
    /// for this long. A slow client that keeps taking bytes is never cut
    /// off, however long a reply takes.
    pub write_stall: Duration,
    /// Bytes of memory held for clients' uploads, as bytes and as trees, on
    /// the whole server: an upload's bytes while its parts arrive, the
    /// records clients stage and have not committed, and an upload's tree
    /// while it is read and used (record lists, query programs, container
    /// data). One client address may hold a quarter of it. Past it a
    /// request is refused with the CORBA system exception `IMP_LIMIT`
    /// before anything is staged, stored or run, and an upload refused part
    /// way is dropped whole.
    pub upload_memory: usize,
    /// Bytes of memory held for the download streams connections keep
    /// paged (a reply larger than one page is kept until its last page is
    /// sent), on the whole server. Past it the streams least recently
    /// paged, on any connection, are dropped (never the one just started),
    /// and a client that continues one is refused with the CORBA system
    /// exception `IMP_LIMIT`.
    pub download_memory: usize,
    /// How long a client disconnected with notice has, once its grace time
    /// ends, to log out and disconnect before the server ends its session
    /// (`ServerHandle::disconnect_client_with_notice`).
    pub logout_wait: Duration,
}

impl ConnectionLimits {
    /// 4096 connections at once, 1024 of them from one address; 30 s for
    /// the TLS handshake; a silent connection closed after an hour with a
    /// session and after 145 s without one; a client that takes nothing
    /// for 2 minutes dropped.
    ///
    /// A client holds a connection for each file it has open, related
    /// files included, and a whole office can reach the server from one
    /// NAT address: 1024 is about 100 users with 10 files each. A client
    /// with a file open sends `Tickle` after 60 s of silence (6.1), so an
    /// hour is 60 missed Tickles, and it leaves room for a client that holds
    /// a session with no file open and so may not tickle. FileMaker Server
    /// closed a silent connection without a session 144.5 s after its last
    /// message, and FileMaker Pro closed one of its own after about 117 s
    /// (6.2): 145 s keeps such connections as long as FileMaker Server did,
    /// and FileMaker Pro still closes its own first. OpenMaker's client
    /// reads what the server sends on its own (callback results) at its
    /// next exchange, at the latest its next Tickle 60 s on: 2 minutes is
    /// twice that.
    ///
    /// 4 GiB held for uploads, 1 GiB of it for one address. A client stages
    /// the records of one commit and commits them, which empties what it
    /// holds. A container is held while its parts arrive and then read into
    /// a tree of about 1.2 times its size, so one address can upload a
    /// container of about 465 MiB at most, and stage a commit of about
    /// 35 MB of short-key records; raise the limit for larger ones. 4 GiB is
    /// not safe on a machine with much less than 8 GB of memory: set it
    /// lower there. Clients behind one NAT address share its quarter.
    ///
    /// 2 GiB of paged downloads. A client pages each download to its end at
    /// once, so what is held is the downloads in progress; the streams
    /// dropped past it are those clients left unfinished longest.
    ///
    /// 30 s to log out after a grace time: FileMaker Server called
    /// `Disconnect` on a client that had closed its files 90 s after a
    /// disconnect with a 60-second grace time (network-protocol.md 6.5;
    /// one observation, so the rule is inferred).
    pub const DEFAULT: ConnectionLimits = ConnectionLimits {
        connections: 4096,
        per_address: 1024,
        handshake: Duration::from_secs(30),
        idle: Some(Duration::from_secs(60 * 60)),
        idle_without_session: Duration::from_secs(145),
        write_stall: Duration::from_secs(120),
        upload_memory: 4 << 30,
        download_memory: 2 << 30,
        logout_wait: Duration::from_secs(30),
    };
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What one calculation of a server script may build
/// ([`Server::with_calc_limits`](crate::Server::with_calc_limits)): text
/// values of at most 256 MiB, and om-calc's nesting of custom function
/// calls and `Evaluate`s. FileMaker documents 2 GB for text, which the
/// desktop client keeps; the smaller limit is OpenMaker Server's own, so
/// that one script cannot take the memory every hosted file needs. Past a
/// limit the calculation ends with error 7, "Running out of memory".
pub const SCRIPT_CALC_LIMITS: CalcLimits = CalcLimits {
    text: 256 << 20,
    ..CalcLimits::DEFAULT
};

/// The engine a server runs for all its files (docs/server-storage.md 5,
/// 11; `--engine`): the file engine, each file one `.fmp12` loaded whole,
/// or the store engine, each file a tree in the store folder's page file,
/// imported from its `.fmp12` when the server starts and hosted read-only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum EngineSettings {
    #[default]
    File,
    Store {
        /// The store folder (`--store-dir`).
        dir: PathBuf,
        /// The page cache's size in bytes (`--store-cache-mib`).
        cache_bytes: u64,
        /// The database folders the store folder must not overlap
        /// ([`check_store_folder`]).
        database_folders: Vec<PathBuf>,
    },
}

/// The store folder the server uses for `store` (`--store-dir`,
/// docs/server-storage.md 11): its parent, which must exist, resolved
/// (`canonicalize`, links in it followed), and its last name, which must be
/// a plain one (a trailing separator is dropped). The server makes at most
/// that one folder. Refused, with what to do, when the parent does not
/// exist or is not a folder, when the last name is not plain, when the
/// folder exists and is not a folder (a link, a file), and when it is a
/// database folder, is inside one or holds one, compared resolved (the
/// store's files would be searched as hosted files, or a hosted file's
/// folder written in). Who may reach the folder the engine checks on the
/// directory it opens (`engine::StoreFolder::open_dir`), which calls this
/// too.
pub fn check_store_folder(store: &Path, database_folders: &[PathBuf]) -> Result<PathBuf, String> {
    let shown = store.display();
    let absolute = std::path::absolute(store).map_err(|e| format!("--store-dir {shown}: {e}"))?;
    let mut components = absolute.components();
    let name = match components.next_back() {
        Some(std::path::Component::Normal(name)) => name.to_owned(),
        _ => {
            return Err(format!(
                "--store-dir {shown} does not end in a folder's name: name the store folder \
                 itself"
            ))
        }
    };
    let parent = components.as_path();
    let parent = match std::fs::canonicalize(parent) {
        Ok(parent) if parent.is_dir() => parent,
        Ok(_) => {
            return Err(format!(
                "--store-dir {shown}: {} is not a folder: give the store folder a parent folder",
                parent.display()
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "--store-dir {shown}: its parent folder {} does not exist: make it first (the \
                 server makes the store folder only)",
                parent.display()
            ))
        }
        Err(e) => return Err(format!("--store-dir {shown}: {}: {e}", parent.display())),
    };
    let store_at = parent.join(name);
    for folder in database_folders {
        let folder_at = resolved(folder);
        let relation = if store_at == folder_at {
            "is"
        } else if store_at.starts_with(&folder_at) {
            "is inside"
        } else if folder_at.starts_with(&store_at) {
            "holds"
        } else {
            continue;
        };
        return Err(format!(
            "--store-dir {} {relation} the database folder {}: give the store a folder of its \
             own, outside every database folder",
            store_at.display(),
            folder_at.display()
        ));
    }
    if std::fs::symlink_metadata(&store_at).is_ok_and(|metadata| !metadata.is_dir()) {
        return Err(format!(
            "--store-dir {} is not a folder: give it a new or empty folder",
            store_at.display()
        ));
    }
    Ok(store_at)
}

/// `path` made absolute against the current folder, resolved as far as it
/// exists, the rest appended as given.
fn resolved(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut rest = Vec::new();
    let mut at = absolute.clone();
    loop {
        if let Ok(real) = std::fs::canonicalize(&at) {
            return rest.iter().rev().fold(real, |path, name| path.join(name));
        }
        match (at.file_name().map(ToOwned::to_owned), at.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                at = parent.to_path_buf();
            }
            _ => return absolute,
        }
    }
}

/// Everything a server needs to start. Its limits and script schedules are
/// given to the [`crate::Server`] it starts (`with_script_limits`,
/// `with_connection_limits`, `with_schedules`). Logins are checked against
/// each hosted file's own accounts and passwords, as FileMaker Server
/// checks them; the server has no accounts of its own.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub identity: ServerIdentity,
    pub files: Vec<HostedFile>,
    /// Host files FileMaker Server calls insecure, which it refuses by
    /// default (`hosting::Insecure`): FileMaker Server's
    /// `fmsadmin set serverconfig securefilesonly=false`. Anyone who
    /// reaches the server can then log in to such a file with full access.
    pub insecure_files: bool,
    /// Record every connection's messages under this directory (see
    /// `record`), for protocol analysis. The recordings hold client license
    /// keys and obscured credentials; keep them out of version control.
    pub record_dir: Option<PathBuf>,
}

#[cfg(test)]
mod store_folder_tests {
    use super::check_store_folder;

    /// docs/server-storage.md 11: a store folder that is a database folder,
    /// is inside one or holds one is refused, also named relative to the
    /// current folder (a new `Store` under `.`) or through `..`; so is one
    /// whose parent does not exist, with nothing made; a folder of its own
    /// is taken, by its parent resolved and its own name.
    #[test]
    fn a_store_folder_that_is_not_the_stores_own_is_refused() {
        let base = std::env::temp_dir().join(format!(
            "om-store-folder-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let databases = base.join("Databases");
        std::fs::create_dir_all(databases.join("sub")).unwrap();
        let folders = [databases.clone()];
        for store in [
            databases.clone(),
            databases.join("Store"),
            base.clone(),
            databases.join("sub/../Store"),
        ] {
            let refused = check_store_folder(&store, &folders);
            assert!(refused.is_err(), "{store:?}");
        }
        // A parent that does not exist: refused, and nothing is made,
        // through `..` into the database folder too.
        for store in [
            base.join("new-parent/../Databases/Store"),
            base.join("existing-not/new-parent/Store"),
        ] {
            let refused = check_store_folder(&store, &folders);
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|why| why.contains("make it first")),
                "{store:?}: {refused:?}"
            );
        }
        assert!(!base.join("new-parent").exists());
        assert!(!base.join("existing-not").exists());
        let own = base.join("Store/");
        assert_eq!(check_store_folder(&own, &folders), Ok(base.join("Store")));
        // Relative to the current folder: `--database-folder .` and a
        // `--store-dir Store` that does not exist yet.
        let cwd = std::env::current_dir().unwrap();
        let relative = [std::path::PathBuf::from(".")];
        let new = std::path::Path::new("om-store-folder-not-made");
        assert!(check_store_folder(new, &relative).is_err());
        assert!(check_store_folder(&cwd.join(new), &relative).is_err());
        assert!(check_store_folder(std::path::Path::new(".."), &[]).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
