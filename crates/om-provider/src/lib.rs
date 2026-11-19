//! Native FileMaker Server data provider. No sample records or layout data are
//! substituted for server results.

mod asset_recovery;
mod authoring;
mod browse;
mod calc;
mod choice;
mod containers;
mod desktop;
mod external;
mod found;
mod library;
mod local;
mod preview;
mod privileges;
mod records;
mod refusals;
mod related_lists;
mod related_owner;
mod scalar_recovery;
mod scripts;
mod security;
mod transaction;
mod value_lists;
mod values;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, GeomRect, HostedFile, LayoutGeometry,
    LayoutInfo, ObjectGeom, PartGeom, Record, RecordPage, RenderLayout,
};
use om_hbam::{serial, Catalog, KeySegment, KeyTree, RidList, SlotReader, Table};
use om_protocol::{LoginEnvironment, MachineInfo, NodeState};
use om_session::Session;

mod schema;
pub use authoring::{Authoring, AuthoringError, ScriptDefinitions};
pub use calc::local_utc_offset;
pub use choice::ListedChoices;
pub use schema::{schema_from_tree, schema_from_tree_with, DatabaseSchemaSnapshot};

/// TLS identity and connection configuration. This deliberately has no `Debug`
/// implementation: its private key and license must never enter diagnostics.
#[derive(Clone)]
pub struct NativeConfig {
    pub client_cert_pem: Vec<u8>,
    pub client_key_pem: Vec<u8>,
    pub additional_ca_pem: Vec<u8>,
    pub callback_host: String,
    pub machine: MachineInfo,
}

impl NativeConfig {
    /// Read explicitly configured identity files, or generate an in-memory
    /// mutual-TLS identity. Server identity is always verified against the
    /// public roots and optional `OPENMAKER_CA_CERT` PEM file.
    pub fn from_env() -> Result<Self, DataError> {
        let cert = std::env::var_os("OPENMAKER_CLIENT_CERT");
        let key = std::env::var_os("OPENMAKER_CLIENT_KEY");
        let (client_cert_pem, client_key_pem) =
            match (cert, key) {
                (Some(cert), Some(key)) => (
                    read_config_file(cert, "client certificate")?,
                    read_config_file(key, "client private key")?,
                ),
                (None, None) => {
                    let identity = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()])
                        .map_err(|e| failure("generate client TLS identity", e))?;
                    (
                        identity.cert.pem().into_bytes(),
                        identity.key_pair.serialize_pem().into_bytes(),
                    )
                }
                _ => return Err(DataError::new(
                    "Set both OPENMAKER_CLIENT_CERT and OPENMAKER_CLIENT_KEY, or leave both unset.",
                )),
            };
        let additional_ca_pem = std::env::var_os("OPENMAKER_CA_CERT")
            .map(|p| read_config_file(p, "additional CA certificate"))
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            client_cert_pem,
            client_key_pem,
            additional_ca_pem,
            callback_host: std::env::var("OPENMAKER_CALLBACK_HOST")
                .unwrap_or_else(|_| "127.0.0.1".into()),
            machine: MachineInfo::default(),
        })
    }
}

fn read_config_file(path: std::ffi::OsString, kind: &str) -> Result<Vec<u8>, DataError> {
    std::fs::read(path).map_err(|e| failure(&format!("read {kind}"), e))
}

/// Desktop logins share the cached display name used by calculations and
/// auto-entry. Account credentials and the generic protocol defaults are
/// independent of this client identity.
fn login_environment(host: &str, file: &str) -> LoginEnvironment {
    let mut environment = LoginEnvironment::for_remote_file(host, file);
    environment.session_name = om_data::host::machine::user_name();
    environment
}

fn failure(context: &str, error: impl std::fmt::Display) -> DataError {
    DataError::new(format!("{context}: {error}"))
}

/// An edit of layout objects that failed, as the window shows it: a
/// refusal keeps the host's status as its code. `sent` names what was sent
/// ("The deletion was").
fn object_edit_error(error: authoring::AuthoringError, sent: &str) -> DataError {
    use om_protocol::authoring::status;
    match error {
        authoring::AuthoringError::Refused {
            status, message, ..
        } => DataError::with_code(i32::from(status), message),
        authoring::AuthoringError::Unavailable(reason) => {
            DataError::with_code(i32::from(status::UNSUPPORTED), reason)
        }
        authoring::AuthoringError::NotSent(reason) => DataError::new(reason),
        authoring::AuthoringError::OutcomeUnknown(reason) => DataError::new(format!(
            "{sent} sent but no answer came back ({reason}), so whether the host saved the \
             change is not known. The layout is read again from the host."
        )),
    }
}

/// A native provider using the same RCFMP/HBAM path as the desktop
/// client. A successful `open` means authentication and catalog loading both
/// completed; there is no sample-data fallback.
pub struct NativeProvider {
    config: NativeConfig,
    session: Option<Session>,
    /// The open file's related files, each on a connection of its own
    /// (external.rs).
    external: external::ExternalFiles,
    catalog: Catalog,
    schema_downloads: Vec<Download>,
    /// The opened file's own calculation schema from its table catalogs
    /// alone, which each related owner's plan imports (`routed_plan`): built
    /// once rather than once for each owner file a layout shows, and
    /// dropped wherever what it is built from changes: `schema_downloads`,
    /// `catalog` (its layouts included) and the file's name, at open, a
    /// schema or layout catalog read, a layout member read, and close.
    own_schema: std::cell::RefCell<Option<std::sync::Arc<om_data::host::CalcSchema>>>,
    layout_downloads: Vec<Download>,
    /// The layouts the layout catalog's index lists whose members the host
    /// did not have when read (#1019): not read again until the index is.
    absent_layouts: BTreeSet<u64>,
    /// The themes a layout named whose members the host did not have when
    /// read, the theme catalog's index read alone ([`Self::unread_theme`]):
    /// not asked for again until the layout catalogs are read again.
    absent_themes: BTreeSet<u64>,
    /// The file's node `[2]` as its open read it, before Login, until the
    /// first calculation schema takes it in place of a download of its own
    /// (network-protocol.md 5.6.9b). Dropped with the schema
    /// ([`Self::drop_calc_schema`]), so a schema built after a refresh reads
    /// `[2]` again, as every later one does.
    status_download: Option<Download>,
    recovery_creation: Option<u32>,
    /// The next write transaction id (write-path.md 1.2).
    next_transaction: u32,
    account_name: String,
    account_info: om_data::host::AccountInfo,
    peer_auth: Option<PeerAuth>,
    desktop: std::sync::Arc<std::sync::Mutex<desktop::DesktopGlobals>>,
    desktop_generation: u64,
    /// The files that refused this session's credentials (refusals.rs),
    /// shared with the peers it opens and their related-file loaders.
    login_refusals: refusals::Shared,
    /// The refusal of the Login [`Self::open_at`] last sent, for the
    /// provider that opened this one as a peer to remember.
    login_refusal: Option<DataError>,
    /// Open a local file asking for an account, without File Options'
    /// "Log in using" ([`DataProvider::open_local_asking`]).
    ask_for_account: bool,
    /// The account and password a local file's Login refused because the
    /// password must be changed first (211), for
    /// [`DataProvider::change_due_password`]; erased when dropped.
    password_change_due: Option<(String, zeroize::Zeroizing<String>)>,
    privileges: privileges::CatalogPrivileges,
    /// The host's version string from the Connect reply.
    host_version: String,
    /// The modification count of each record as last read, by (table,
    /// record), to refuse saving over another client's change.
    mod_counts: BTreeMap<(u64, u64), u64>,
    /// The checked version of a retained draft, by window, table and record.
    /// `None` means the draft has no accepted version: refuse its save
    /// until a successful explicit readback establishes one.
    pinned: BTreeMap<(u32, u64, u64), Option<u64>>,
    /// Each script definition's content revision as the scripts were last
    /// read, by id: a step save must find the script still there
    /// (scripts.rs).
    script_revisions: BTreeMap<u64, om_protocol::authoring::Revision>,
    /// Whether this connection may replace script steps, asked once per
    /// connection (scripts.rs).
    script_saving: Option<authoring::Authoring>,
    /// The script catalog index's revision when the Script Workspace last
    /// read the scripts: its catalog changes are made on that catalog
    /// (scripts.rs).
    script_edit_base: Option<om_protocol::authoring::Revision>,
    /// The calculation schema and the last page's records (calc.rs).
    calc: calc::CalcState,
    /// The window's found set of each table (found.rs), and the other
    /// windows' by window id.
    found: found::FoundSets,
    window: u32,
    windows: BTreeMap<u32, found::FoundSets>,
    /// What a local file's saved found sets restored (found.rs).
    saved_found: found::SavedFound,
    /// The open script transaction (transaction.rs), or the current
    /// window's record session.
    transaction: Option<transaction::Transaction>,
    /// The related file's transaction it opened (related_owner.rs).
    owner_transaction: related_owner::OwnerTransaction,
    /// The other windows' record sessions (`DataProvider::begin_record_commit`),
    /// with their related files' transactions, by window: a window's
    /// session goes with it ([`Self::use_window`]), as FileMaker Pro binds
    /// the transaction to the window (script-runtime.md 8.1).
    record_sessions: BTreeMap<u32, transaction::SetAside>,
    /// Record sessions of new records a reconnect carries, copied from the
    /// lost session, while no reopen has succeeded (#152).
    carried: Vec<transaction::Carried>,
    asset_proofs: asset_recovery::Proofs,
    pending_assets: asset_recovery::Pending,
    /// Ordinary entry may proceed without recovery proof. Its accepted edit
    /// must never later become a freshly captured before-work baseline.
    unprepared_root_edits: BTreeSet<(u32, u64, i64)>,
    scalar_refusal: Option<scalar_recovery::Receipt>,
    scalar_carry: Option<scalar_recovery::ScalarCarry>,
    asset_settlement: Option<scalar_recovery::ScalarCarry>,
    /// What the last reconnect did with them, until asked
    /// (`DataProvider::take_recreated_records`).
    recreated: Vec<om_data::RecreatedRecord>,
    /// A window's carried new records a reopen could not hold again, kept
    /// as they were until the window tries again or discards them.
    unrecreated: BTreeMap<u32, transaction::Transaction>,
    /// The transaction a window's set-aside record session was entered
    /// over (`DataProvider::enter_record_session`), until it is left.
    entered_over: Option<transaction::SetAside>,
    /// New records' previews, by id (preview.rs).
    previews: preview::Previews,
    /// As an owning file written by the opened file (related_owner.rs,
    /// #187): the schema its records are planned over, with the opened
    /// file's tables imported, while such a write or commit runs.
    routed_plan: Option<std::sync::Arc<external::OwnerPlan>>,
    /// And the opened file's reads of those tables, through its own
    /// session (moved here for the call and back).
    caller: Option<related_owner::CallerReads>,
    /// How long an open file's connection may stay silent before the
    /// session tickles it (`om_session::KEEPALIVE_INTERVAL`).
    keepalive: Option<Duration>,
    /// A hosted file's related files open when something reads them
    /// ([`Self::with_related_on_demand`], calc/demand.rs).
    related_on_demand: bool,
    /// [`Self::with_related_retry_after`].
    related_retry_after: Option<Duration>,
    /// The window's queued Closes: opening related files stops at one.
    close_requests: om_data::CloseRequests,
    /// This provider's lease of the private host of the file it opened
    /// from this computer ([`DataProvider::open_local`]); while it asks for
    /// an account, the host waits here without a session.
    local: Option<local::Lease>,
    /// The hosts of the files on this computer this provider's family has
    /// open: its own, its peers' and its related files' (local.rs).
    local_hosts: std::sync::Arc<local::LocalHosts>,
    /// The account and password a file opened from this computer was opened
    /// with, kept while it is open to log in to the files beside it that
    /// its scripts open (`open_peer`); the password is erased when dropped
    /// and is not kept after a reconnect.
    local_auth: Option<LocalAuth>,
    /// Manage Database's open edit and the schema locks it holds
    /// (schema.rs).
    schema_edit: Option<schema::SchemaEdit>,
    /// The files its commits have written, by the id each was given
    /// (`om_data::ParticipantId`, related_owner.rs).
    participants: Vec<related_owner::FileKey>,
    /// Windows' commits that may be partly applied, kept until settled
    /// (related_owner.rs, #1006).
    unsettled: Vec<related_owner::Unsettled>,
}

impl NativeProvider {
    /// The account and password the file is open with, while the provider
    /// keeps them for its scripts' opens, including after a reconnect.
    /// The account `file` on `host` at `port`, another host or port than
    /// this file's, is sent with no account typed for it, if any:
    /// - one that host admitted in this session, for a related file
    ///   (typed for it, or this file's, `ExternalFiles::accepted_on`),
    ///   unless `file` refused it;
    /// - else this file's own, offered there once, as FileMaker Pro offers
    ///   the account a file was opened with to another host before it asks
    ///   (network-protocol.md 5.5): not after any file there refused it in
    ///   this session (refusals.rs), nor if this file no longer holds its
    ///   password.
    ///
    /// It goes only on a connection to that host, whose certificate is
    /// verified before the Login, as every Open's (`open_catalog`). `None`:
    /// the user is asked for one ([`DataProvider::peer_refused`]).
    fn other_host_account(
        &self,
        host: &str,
        port: u16,
        file: &str,
    ) -> Option<(String, zeroize::Zeroizing<String>)> {
        let refusals = self.login_refusals.lock().expect("login refusals");
        if let Some((account, password)) = self.external.accepted_on(host, port) {
            let key = refusals::FileKey::hosted(host, port, file);
            return refusals
                .refused(&key, account, password)
                .is_none()
                .then(|| {
                    (
                        account.to_owned(),
                        zeroize::Zeroizing::new(password.to_owned()),
                    )
                });
        }
        let PeerAuth {
            account,
            password: Some(password),
            ..
        } = self.peer_auth.as_ref()?
        else {
            return None;
        };
        refusals
            .refused_on(host, port, account, password)
            .is_none()
            .then(|| (account.clone(), password.clone()))
    }

    pub(crate) fn held_credentials(&self) -> Option<(String, zeroize::Zeroizing<String>)> {
        let (account, password) = match (&self.peer_auth, &self.local_auth) {
            (Some(auth), _) => (&auth.account, auth.password.as_ref()?),
            (None, Some(auth)) => (&auth.account, auth.password.as_ref()?),
            (None, None) => return None,
        };
        Some((account.clone(), password.clone()))
    }
}

/// Where a hosted file's peer `file` is: on `auth`'s host and port by its
/// name, or, named as `fmnet:/host:port/Name`
/// ([`om_data::file_refs::fmnet_name`]), on that host and port, and then
/// whether that is another than `auth`'s (the host's text in any case).
fn peer_target(auth: &PeerAuth, file: &str) -> (String, u16, String, bool) {
    match om_data::file_refs::fmnet_target(file) {
        Some((host, port, name)) => {
            let elsewhere = !(port == auth.port && host.eq_ignore_ascii_case(&auth.host));
            (
                host,
                port,
                om_data::file_refs::hosted_name(&name),
                elsewhere,
            )
        }
        None => (auth.host.clone(), auth.port, file.to_owned(), false),
    }
}

/// Opening `file` on `host` at `port`, another host or port than the open
/// file's, with no account typed for it, when none is sent there
/// ([`NativeProvider::other_host_account`]): the host is not contacted, and
/// a script step asks for an account ([`DataProvider::peer_refused`] is
/// true).
fn another_host(host: &str, port: u16, file: &str) -> DataError {
    DataError::with_code(
        212,
        format!(
            "“{file}” is on another host ({host}:{port}), which did not admit the account this \
             file was opened with: an account of that host's is needed."
        ),
    )
}

/// The credentials a local file opens its peers with. Never in `Debug`.
struct LocalAuth {
    account: String,
    password: Option<zeroize::Zeroizing<String>>,
}

/// The connection [`NativeProvider::open_at`] opens a file on.
enum Via {
    /// One of its own.
    Own,
    /// A handle on one already made to the file's host: the file opened
    /// there for the same account is held with it; held for another, or
    /// being opened, a connection of its own (#1011).
    Shared(Session),
    /// Only that one ([`DataProvider::reopen_on_successor`]): a file that
    /// cannot be held or opened there is not opened.
    Only(Session),
}

/// Where [`NativeProvider::open_at`] connects: the CA to trust, the
/// machine info Connect sends, and for a file opened from this computer,
/// its path (its private host is `NativeProvider::local`).
struct Endpoint {
    ca_pem: Vec<u8>,
    machine: MachineInfo,
    local: Option<std::path::PathBuf>,
}

/// FileMaker's error 212, "Invalid user account or password": a local
/// file refused the account.
const LOGIN_REFUSED: i32 = 212;

/// A local file's refusals of an account whose password was right
/// (`om_server::LOGIN_INACTIVE_ACCOUNT` and the rest).
const LOGIN_INACTIVE_ACCOUNT: i32 = om_server::LOGIN_INACTIVE_ACCOUNT as i32;
const LOGIN_PASSWORD_CHANGE_DUE: i32 = om_server::LOGIN_PASSWORD_CHANGE_DUE as i32;
const LOGIN_UNREADABLE_ACCOUNT: i32 = om_server::LOGIN_UNREADABLE_ACCOUNT as i32;
const LOGIN_UNSUPPORTED_PRIVILEGES: i32 = om_server::LOGIN_UNSUPPORTED_PRIVILEGES as i32;

/// FileMaker's error 214, "Too many login attempts": a host refused the
/// login without checking it, after too many failures
/// (`om_server::LOGIN_TOO_MANY_ATTEMPTS`).
const LOGIN_TOO_MANY_ATTEMPTS: i32 = om_server::LOGIN_TOO_MANY_ATTEMPTS as i32;

/// Whether a local file's Login error leaves the file waiting for another
/// account.
fn local_refusal(code: i32) -> bool {
    matches!(
        code,
        LOGIN_REFUSED
            | LOGIN_INACTIVE_ACCOUNT
            | LOGIN_PASSWORD_CHANGE_DUE
            | LOGIN_UNREADABLE_ACCOUNT
            | LOGIN_UNSUPPORTED_PRIVILEGES
    )
}

/// Whether `path` names the file at `opened` (a local host's own path).
fn same_file(opened: &std::path::Path, path: &std::path::Path) -> bool {
    std::fs::canonicalize(path).is_ok_and(|path| path == opened)
}

// Retained only while the desktop file is open. Never exposed through Debug.
struct PeerAuth {
    host: String,
    port: u16,
    account: String,
    password: Option<zeroize::Zeroizing<String>>,
}

pub(crate) use om_data::{FIELD_NOT_MODIFIABLE, PRIVILEGE_DENIED};

impl NativeProvider {
    pub fn new(config: NativeConfig) -> Self {
        Self {
            config,
            session: None,
            external: external::ExternalFiles::default(),
            catalog: Catalog::default(),
            schema_downloads: Vec::new(),
            own_schema: Default::default(),
            layout_downloads: Vec::new(),
            absent_layouts: BTreeSet::new(),
            absent_themes: BTreeSet::new(),
            status_download: None,
            recovery_creation: None,
            next_transaction: 1,
            account_name: String::new(),
            account_info: Default::default(),
            peer_auth: None,
            desktop: Default::default(),
            desktop_generation: 0,
            login_refusals: Default::default(),
            login_refusal: None,
            ask_for_account: false,
            password_change_due: None,
            privileges: privileges::CatalogPrivileges::default(),
            host_version: String::new(),
            mod_counts: BTreeMap::new(),
            pinned: BTreeMap::new(),
            script_revisions: BTreeMap::new(),
            script_saving: None,
            script_edit_base: None,
            calc: calc::CalcState::default(),
            found: found::FoundSets::new(),
            window: 1,
            windows: BTreeMap::new(),
            saved_found: found::SavedFound::default(),
            transaction: None,
            record_sessions: BTreeMap::new(),
            carried: Vec::new(),
            asset_proofs: Default::default(),
            pending_assets: Default::default(),
            unprepared_root_edits: Default::default(),
            scalar_refusal: None,
            scalar_carry: None,
            asset_settlement: None,
            recreated: Vec::new(),
            unrecreated: BTreeMap::new(),
            entered_over: None,
            owner_transaction: Default::default(),
            previews: preview::Previews::default(),
            routed_plan: None,
            caller: None,
            keepalive: Some(om_session::KEEPALIVE_INTERVAL),
            related_on_demand: false,
            related_retry_after: None,
            close_requests: om_data::CloseRequests::default(),
            local: None,
            local_hosts: Default::default(),
            local_auth: None,
            schema_edit: None,
            participants: Vec::new(),
            unsettled: Vec::new(),
        }
    }

    pub fn from_env() -> Result<Self, DataError> {
        NativeConfig::from_env().map(Self::new)
    }

    /// Tickle an open file's connection after `interval` of silence instead
    /// of FileMaker's 60 s (tests); `None` sends no Tickle.
    #[must_use]
    pub fn with_keepalive(mut self, interval: Option<Duration>) -> Self {
        self.keepalive = interval;
        self
    }

    /// Open a hosted file's related files when a layout or calculation
    /// first reads them, rather than all of them when the calculation
    /// schema is first built (calc/demand.rs, #983). Files on this computer
    /// open theirs when the schema is built either way.
    pub fn with_related_on_demand(mut self, on: bool) -> Self {
        self.related_on_demand = on;
        self
    }

    /// How long a related file whose host could not be reached waits before
    /// a read tries it again (#740); 30 seconds unless set.
    pub fn with_related_retry_after(mut self, after: Duration) -> Self {
        self.related_retry_after = Some(after);
        self
    }

    /// [`Self::with_related_retry_after`] for an open file, from its next
    /// read on.
    pub fn set_related_retry_after(&mut self, after: Duration) {
        self.related_retry_after = Some(after);
        self.external.set_retry_after(Some(after));
    }

    /// What the user should know about the open file's related files
    /// (external.rs): those that could not be opened, and tables they
    /// cannot read. Known once the calculation schema is built. Their
    /// occurrences stay unresolved; nothing they show is read from another
    /// file's table with the same id. No credentials.
    pub fn related_file_warnings(&self) -> &[String] {
        self.external.warnings()
    }

    fn require_open(&self) -> Result<(), DataError> {
        self.session
            .as_ref()
            .map(|_| ())
            .ok_or_else(|| DataError::new("No hosted file is open."))
    }

    /// Whether the open file is one on this computer that cannot be
    /// written ([`om_server::LocalHost::is_read_only`]).
    fn read_only(&self) -> bool {
        self.session.is_some() && self.local.as_ref().is_some_and(|l| l.is_read_only())
    }

    /// Give up this provider's lease of the local file's host: the host
    /// closes when no other session of the family has the file (local.rs).
    /// When something still holds the file then (a script that did not
    /// stop), that is logged: the file stays in use until it lets go, and
    /// opening it again says so.
    fn release_local(&mut self) {
        if let Some(lease) = self.local.take() {
            lease.release();
        }
    }

    /// Refuse a change to a read-only local file before anything is sent:
    /// FileMaker's error 6, "File is read-only".
    fn require_writable(&self) -> Result<(), DataError> {
        if self.read_only() {
            Err(DataError::with_code(
                6,
                "This file is read-only: changes to it cannot be saved.",
            ))
        } else if !self.records_allowed() {
            // FileMaker's 200, "Record access is denied" (the host refuses
            // the write with it too), with FileMaker Pro's alert text (#137).
            Err(DataError::with_code(200, PRIVILEGE_DENIED))
        } else {
            Ok(())
        }
    }

    /// Refuse `action` on a record of this file's base table `table` when
    /// the account's privilege set does not allow it (200, "Record access
    /// is denied"; the host refuses the write too).
    fn require_action(
        &self,
        table: u64,
        action: om_data::host::security::RecordAction,
    ) -> Result<(), DataError> {
        let allowed = self
            .account_info
            .set_access
            .as_ref()
            .is_none_or(|access| access.may_ever(&om_hbam::slot::uint_segment(table), action));
        if allowed {
            Ok(())
        } else {
            Err(DataError::with_code(200, PRIVILEGE_DENIED))
        }
    }

    /// Refuse `action` on a record of this file's base table `table` whose
    /// values are `row`, when the account's custom set allows the action
    /// only where a formula is true and it is not on `row` (200); also when
    /// the formula cannot be evaluated here. The host decides again on the
    /// whole record at commit.
    pub(crate) fn require_limit(
        &mut self,
        table: u64,
        action: om_data::host::security::RecordAction,
        row: &BTreeMap<u64, String>,
    ) -> Result<(), DataError> {
        let key = om_hbam::slot::uint_segment(table);
        let Some(code) = self
            .account_info
            .set_access
            .as_ref()
            .and_then(|access| access.limit(&key, action))
            .map(<[u8]>::to_vec)
        else {
            return Ok(());
        };
        if self.formula_allows(table, &code, row)? {
            Ok(())
        } else {
            Err(DataError::with_code(200, PRIVILEGE_DENIED))
        }
    }

    /// Refuse an edit of a record of base table `table` whose values, as
    /// this provider holds them, are `row`, when the account's custom set
    /// limits viewing the table's records by a formula that is not true on
    /// them (200): such a record arrived without its values, so writing it
    /// back would erase them (#670). The host refuses it again on the stored
    /// record.
    pub(crate) fn require_viewable(
        &mut self,
        table: u64,
        row: &BTreeMap<u64, String>,
    ) -> Result<(), DataError> {
        let key = om_hbam::slot::uint_segment(table);
        let Some(code) = self
            .account_info
            .set_access
            .as_ref()
            .and_then(|access| access.view_limit(&key))
            .map(<[u8]>::to_vec)
        else {
            return Ok(());
        };
        if self.formula_allows(table, &code, row)? {
            Ok(())
        } else {
            Err(DataError::with_code(200, PRIVILEGE_DENIED))
        }
    }

    /// Whether limiting formula `code` of base table `table` is true on
    /// `row`; an error (200) when it cannot be evaluated here.
    fn formula_allows(
        &mut self,
        table: u64,
        code: &[u8],
        row: &BTreeMap<u64, String>,
    ) -> Result<bool, DataError> {
        let file = self.calc_schema_cached()?;
        let tree = download_tree(&self.schema_downloads)?;
        let definition = self
            .catalog
            .table(table)
            .ok_or_else(|| DataError::with_code(200, "The record's table is missing."))?;
        let limit = om_data::rules::recalc::RecordLimit::new(definition, &tree, &file, code, None)
            .map_err(|e| {
                DataError::with_code(
                    200,
                    format!(
                        "This account's privilege set limits that change by a calculation \
                         OpenMaker cannot evaluate: {e}"
                    ),
                )
            })?;
        let environment = self.record_environment()?;
        Ok(limit.allows(row, &environment))
    }

    /// Whether the account's privilege set lets it read value list `id`.
    pub(crate) fn shows_value_list(&self, id: u64) -> bool {
        self.account_info
            .set_access
            .as_ref()
            .is_none_or(|access| access.shows_value_list(id))
    }

    /// The account's access to records through layout `layout_id`
    /// (session-identity.md 2.2): modifiable unless a custom set says view
    /// only or no access.
    fn layout_records(&self, layout_id: u32) -> om_data::host::privilege_set::AccessLevel {
        self.account_info.set_access.as_ref().map_or(
            om_data::host::privilege_set::AccessLevel::Modifiable,
            |access| access.layout_records(u64::from(layout_id)),
        )
    }

    /// Refuse a write through layout `layout_id` whose records the account's
    /// custom set does not let it change (view only or no access; 200, with
    /// FileMaker Pro's alert text).
    fn require_layout_records(&self, layout_id: u32) -> Result<(), DataError> {
        if self.layout_records(layout_id) == om_data::host::privilege_set::AccessLevel::Modifiable {
            Ok(())
        } else {
            Err(DataError::with_code(200, PRIVILEGE_DENIED))
        }
    }

    /// Refuse a script's write through layout `layout_id` (Set Field, a
    /// script's commit): through records view only (200, #705); through
    /// records without access, a script writes as the table's rules allow,
    /// as FileMaker Pro 26.0.3 lets it (#707's reading).
    fn require_script_layout_records(&self, layout_id: u32) -> Result<(), DataError> {
        if self.layout_records(layout_id) == om_data::host::privilege_set::AccessLevel::ViewOnly {
            Err(DataError::with_code(200, PRIVILEGE_DENIED))
        } else {
            Ok(())
        }
    }

    /// Whether the account's privilege set lets it see layout `layout_id`.
    pub(crate) fn shows_layout(&self, layout_id: u64) -> bool {
        self.account_info
            .set_access
            .as_ref()
            .is_none_or(|access| access.shows_layout(layout_id))
    }

    /// Whether the account's privilege set lets it create, edit and delete
    /// records: not [Read-Only Access] (session-identity.md 2.1). A set the
    /// host did not verify as built in leaves this to the host.
    fn records_allowed(&self) -> bool {
        self.account_info
            .set_access
            .as_ref()
            .is_none_or(om_data::host::security::SetAccess::edits_any_record)
    }

    /// [`DataProvider::place_layout_field`] without the refresh: the layout
    /// read now, its revision checked against the expected one, the
    /// placement planned as the host plans it (`om_data::layout_placement`),
    /// then sent as `om-author`'s `place_field` sends it.
    fn place_field_on_layout(
        &mut self,
        place: &om_data::layout_placement::LayoutFieldPlacement,
    ) -> Result<om_data::layout_placement::PlacedLayoutField, DataError> {
        use om_data::layout_placement::{self, PlacedLayoutField, Refusal};
        use om_protocol::authoring::{catalog_revision, status};
        let layout = u64::from(place.layout);
        let member = self.layout_member(layout)?.ok_or_else(|| {
            DataError::with_code(
                i32::from(status::LAYOUT_MISSING),
                format!("The host has no layout {layout}."),
            )
        })?;
        let current = catalog_revision(&member);
        let expected = place.expected;
        if expected != current {
            // A lost answer's objects may already be there; only the host
            // can tell (it checks the field's table again), so the request
            // goes with the expected revision. Otherwise it is a conflict.
            if layout_placement::already_placed(&member, &place.placement).is_none() {
                return Err(DataError::with_code(
                    layout_placement::CONFLICT,
                    "The layout changed since it was shown; nothing was saved. It is read \
                     again: place the field on it again.",
                ));
            }
        } else {
            layout_placement::plan(&member, &place.placement).map_err(|refusal| match refusal {
                Refusal::Invalid(reason) => {
                    DataError::with_code(i32::from(status::INVALID), reason)
                }
                Refusal::Unsupported(reason) => {
                    DataError::with_code(i32::from(status::UNSUPPORTED), reason)
                }
            })?;
        }
        // The field's identity is its table's and its own: an occurrence
        // that shows another table now, whose field has the same id, is
        // refused, not given that field. The host checks the table again
        // under its lock.
        let occurrence = place.placement.occurrence;
        let table = place.table;
        let shown = self
            .database_schema()?
            .occurrences
            .iter()
            .find(|o| o.id == occurrence)
            .and_then(|o| o.table_id);
        if shown != Some(table) {
            return Err(DataError::with_code(
                layout_placement::CONFLICT,
                "The layout's table occurrence no longer shows the table the field was chosen \
                 from; nothing was saved. Open the file again to choose from its current \
                 tables.",
            ));
        }
        let frame = |r: &om_layout::Rect| [r.top, r.left, r.bottom, r.right];
        let placed = self
            .place_field(om_protocol::authoring::PlaceField {
                layout,
                expected,
                table,
                occurrence,
                field: place.placement.field,
                frame: frame(&place.placement.frame),
                label: place
                    .placement
                    .label
                    .as_ref()
                    .map(|label| (label.text.clone(), frame(&label.frame))),
            })
            .map_err(|error| match error {
                authoring::AuthoringError::Refused {
                    status, message, ..
                } => DataError::with_code(i32::from(status), message),
                authoring::AuthoringError::Unavailable(reason) => {
                    DataError::with_code(i32::from(status::UNSUPPORTED), reason)
                }
                authoring::AuthoringError::NotSent(reason) => DataError::new(reason),
                authoring::AuthoringError::OutcomeUnknown(reason) => DataError::new(format!(
                    "The placement was sent but no answer came back ({reason}), so it may \
                     or may not have been made. The layout is read again from the host."
                )),
            })?;
        Ok(PlacedLayoutField {
            changed: placed.changed,
            field_object: placed.field_object,
            label_object: Some(placed.label_object).filter(|&key| key != 0),
            revision: placed.revision,
        })
    }

    /// [`DataProvider::set_layout_object_frames`] without the refresh: the
    /// layout checked as the host checks it ([`Self::check_object_edit`]),
    /// then the frames sent as `om-author`'s `set_object_frames` sends them.
    fn set_frames_on_layout(
        &mut self,
        change: &om_data::layout_objects::LayoutObjectFrames,
    ) -> Result<om_data::layout_objects::FramedObjects, DataError> {
        use om_data::layout_objects;
        let layout = u64::from(change.layout);
        self.check_object_edit(
            layout,
            &change.expected,
            |member| layout_objects::already_framed(member, &change.frames),
            |member| layout_objects::plan(member, &change.frames),
            "move the objects again",
        )?;
        let edges = |r: &om_layout::Rect| [r.top, r.left, r.bottom, r.right];
        let framed = self
            .set_object_frames(om_protocol::authoring::SetObjectFrames {
                layout,
                expected: change.expected,
                frames: change
                    .frames
                    .iter()
                    .map(|f| (f.object, edges(&f.frame)))
                    .collect(),
            })
            .map_err(|error| object_edit_error(error, "The moved objects were"))?;
        Ok(layout_objects::FramedObjects {
            changed: framed.changed,
            revision: framed.revision,
        })
    }

    /// [`DataProvider::delete_layout_objects`] without the refresh: the
    /// layout checked as the host checks it ([`Self::check_object_edit`]),
    /// then the deletion sent as `om-author`'s `delete_objects` sends it.
    fn delete_on_layout(
        &mut self,
        change: &om_data::layout_objects::LayoutObjectDeletion,
    ) -> Result<om_data::layout_objects::DeletedObjects, DataError> {
        use om_data::layout_objects;
        let layout = u64::from(change.layout);
        self.check_object_edit(
            layout,
            &change.expected,
            |member| layout_objects::already_deleted(member, &change.objects),
            |member| layout_objects::plan_deletion(member, &change.objects),
            "delete the objects again",
        )?;
        let deleted = self
            .delete_objects(om_protocol::authoring::DeleteObjects {
                layout,
                expected: change.expected,
                objects: change.objects.clone(),
            })
            .map_err(|error| object_edit_error(error, "The deletion was"))?;
        Ok(layout_objects::DeletedObjects {
            changed: deleted.changed,
            revision: deleted.revision,
        })
    }

    /// Check an edit of layout `layout`'s objects against the layout read
    /// now, as the host checks it: at revision `expected`, the edit must
    /// plan (`plan`); past it, it is a conflict unless the layout already
    /// holds the edit (`already`), from a request whose answer was lost,
    /// which the host is asked to tell. `again` ends the conflict's
    /// message ("move the objects again").
    fn check_object_edit(
        &mut self,
        layout: u64,
        expected: &om_protocol::authoring::Revision,
        already: impl Fn(&om_hbam::OwnedNode) -> bool,
        plan: impl Fn(
            &om_hbam::OwnedNode,
        )
            -> Result<om_data::layout_objects::Planned, om_data::layout_placement::Refusal>,
        again: &str,
    ) -> Result<(), DataError> {
        use om_data::layout_placement::{self, Refusal};
        use om_protocol::authoring::{catalog_revision, status};
        let member = self.layout_member(layout)?.ok_or_else(|| {
            DataError::with_code(
                i32::from(status::LAYOUT_MISSING),
                format!("The host has no layout {layout}."),
            )
        })?;
        if catalog_revision(&member) != *expected {
            if !already(&member) {
                return Err(DataError::with_code(
                    layout_placement::CONFLICT,
                    format!(
                        "The layout changed since it was shown; nothing was saved. It is read \
                         again: {again}."
                    ),
                ));
            }
            return Ok(());
        }
        plan(&member).map(drop).map_err(|refusal| match refusal {
            Refusal::Invalid(reason) => DataError::with_code(i32::from(status::INVALID), reason),
            Refusal::Unsupported(reason) => {
                DataError::with_code(i32::from(status::UNSUPPORTED), reason)
            }
        })
    }

    /// [`DataProvider::create_layout`] without the refresh: the name
    /// checked as the host checks it, the layout catalog's index read now
    /// for its revision, then the request sent as `om-author`'s
    /// `create_layout` sends it.
    fn create_layout_with(
        &mut self,
        request: &om_data::layout_catalog::LayoutRequest,
    ) -> Result<om_data::layout_catalog::CreatedLayout, DataError> {
        use om_data::layout_catalog::{self, CreatedLayout};
        use om_protocol::authoring::{catalog_revision, status};
        layout_catalog::check_name(&request.name).map_err(|refusal| {
            DataError::with_code(i32::from(status::NAME_UNUSABLE), refusal.to_string())
        })?;
        let index = self
            .layout_index()?
            .unwrap_or_else(|| om_hbam::OwnedNode::list(vec![1], Vec::new()));
        // A new identity for each request: a window's two layouts of one
        // name, made from one list, are two layouts.
        let mut uuid = [0u8; 16];
        getrandom::getrandom(&mut uuid)
            .map_err(|e| DataError::new(format!("No random UUID for the layout: {e}")))?;
        let created = self
            .create_layout_on_host(om_protocol::authoring::CreateLayout {
                expected: catalog_revision(&index),
                occurrence: request.occurrence,
                like: request.like.map_or(0, u64::from),
                name: request.name.clone(),
                uuid,
            })
            .map_err(|error| match error {
                authoring::AuthoringError::Refused {
                    status, message, ..
                } => DataError::with_code(i32::from(status), message),
                authoring::AuthoringError::Unavailable(reason) => {
                    DataError::with_code(i32::from(status::UNSUPPORTED), reason)
                }
                authoring::AuthoringError::NotSent(reason) => DataError::new(reason),
                authoring::AuthoringError::OutcomeUnknown(reason) => DataError::new(format!(
                    "The new layout was sent but no answer came back ({reason}), so it may or \
                     may not have been added. The layouts are read again from the host."
                )),
            })?;
        let layout = u32::try_from(created.layout).map_err(|_| {
            DataError::new(format!(
                "The host added layout {}, an id this version does not show.",
                created.layout
            ))
        })?;
        Ok(CreatedLayout {
            layout,
            changed: created.changed,
        })
    }

    /// [`DataProvider::duplicate_layout`] without the refresh: the layout
    /// catalog's index read now for its revision, the copy planned as the
    /// host plans it (`layout_catalog::plan_duplicate`, which refuses what
    /// the host would), then the request sent as `om-author`'s
    /// `duplicate_layout` sends it.
    fn duplicate_layout_with(
        &mut self,
        layout: u32,
    ) -> Result<om_data::layout_catalog::CreatedLayout, DataError> {
        use om_data::layout_catalog::CreatedLayout;
        use om_protocol::authoring::{catalog_revision, status};
        let index = self
            .layout_index()?
            .unwrap_or_else(|| om_hbam::OwnedNode::list(vec![1], Vec::new()));
        let mut uuid = [0u8; 16];
        getrandom::getrandom(&mut uuid)
            .map_err(|e| DataError::new(format!("No random UUID for the layout: {e}")))?;
        let created = self
            .duplicate_layout_on_host(om_protocol::authoring::DuplicateLayout {
                expected: catalog_revision(&index),
                layout: u64::from(layout),
                uuid,
            })
            .map_err(|error| match error {
                authoring::AuthoringError::Refused {
                    status, message, ..
                } => DataError::with_code(i32::from(status), message),
                authoring::AuthoringError::Unavailable(reason) => {
                    DataError::with_code(i32::from(status::UNSUPPORTED), reason)
                }
                authoring::AuthoringError::NotSent(reason) => DataError::new(reason),
                authoring::AuthoringError::OutcomeUnknown(reason) => DataError::new(format!(
                    "The copy was asked for but no answer came back ({reason}), so it may or \
                     may not have been made. The layouts are read again from the host."
                )),
            })?;
        let layout = u32::try_from(created.layout).map_err(|_| {
            DataError::new(format!(
                "The host added layout {}, an id this version does not show.",
                created.layout
            ))
        })?;
        Ok(CreatedLayout {
            layout,
            changed: created.changed,
        })
    }

    /// [`DataProvider::delete_layout`] without the refresh: the layout
    /// catalog's index read now for its revision, the deletion planned as
    /// the host plans it (which refuses what the host would), then sent as
    /// `om-author`'s `delete_layout` sends it.
    fn delete_layout_with(
        &mut self,
        layout: u32,
    ) -> Result<om_data::layout_catalog::RemovedLayout, DataError> {
        use om_data::layout_catalog::RemovedLayout;
        use om_protocol::authoring::{catalog_revision, status};
        let index = self
            .layout_index()?
            .unwrap_or_else(|| om_hbam::OwnedNode::list(vec![1], Vec::new()));
        let deleted = self
            .delete_layout_on_host(om_protocol::authoring::DeleteLayout {
                expected: catalog_revision(&index),
                layout: u64::from(layout),
            })
            .map_err(|error| match error {
                authoring::AuthoringError::Refused {
                    status, message, ..
                } => DataError::with_code(i32::from(status), message),
                authoring::AuthoringError::Unavailable(reason) => {
                    DataError::with_code(i32::from(status::UNSUPPORTED), reason)
                }
                authoring::AuthoringError::NotSent(reason) => DataError::new(reason),
                authoring::AuthoringError::OutcomeUnknown(reason) => DataError::new(format!(
                    "The deletion was asked for but no answer came back ({reason}), so the \
                     layout may or may not have been deleted. The layouts are read again from \
                     the host."
                )),
            })?;
        Ok(RemovedLayout {
            layout,
            changed: deleted.changed,
        })
    }

    /// Read the layout catalogs again, as [`Self::refresh_layout_downloads`],
    /// and the layout list and calculation schema made from them, so a new
    /// layout is listed, opened and named in calculations.
    fn refresh_layouts(&mut self) -> Result<(), DataError> {
        self.refresh_layout_downloads()?;
        self.catalog.layouts = catalog_layouts(&self.layout_downloads)?;
        *self.calc.fonts.get_mut() = Some(file_fonts(&self.layout_downloads)?);
        *self.own_schema.get_mut() = None;
        self.absent_layouts.clear();
        self.absent_themes.clear();
        self.drop_calc_schema();
        Ok(())
    }

    /// Drop the calculation schema, so the next one is built from the
    /// catalogs as read again, the file's node `[2]` included: the node kept
    /// from the open (`status_download`) and the decoded root go too.
    pub(crate) fn drop_calc_schema(&mut self) {
        self.calc.schema = None;
        self.calc.recovery_catalogs = None;
        self.calc.root = None;
        self.status_download = None;
        self.recovery_creation = None;
    }

    /// The file's font families (`[25]`), read now if its open did not read
    /// them: a related file's, whose styled text names a font (#1098).
    pub(crate) fn file_fonts(&mut self) -> Result<Arc<BTreeMap<u64, String>>, DataError> {
        if let Some(fonts) = self.calc.fonts.get_mut() {
            return Ok(Arc::clone(fonts));
        }
        let fonts = Arc::new(calc::read_fonts(self.session_mut()?)?);
        *self.calc.fonts.get_mut() = Some(Arc::clone(&fonts));
        Ok(fonts)
    }

    /// Read layout `id`'s member `[4, 5, L]` when only the catalog's index
    /// was read for it (on demand, #1019), and grow the calculation schema
    /// with what it holds: its objects, parts and fields. Nothing when the
    /// member, or the whole catalog, was read already. When the host has no
    /// such member the layout is settled as having no objects, and its
    /// decoding says the host has no such layout.
    pub(crate) fn ensure_layout_member(&mut self, id: u64) -> Result<(), DataError> {
        let index = [key_bytes(4), vec![1]];
        let member = vec![key_bytes(4), vec![5], key_bytes(id)];
        let read = |d: &Download| d.path == member || d.path == [key_bytes(4)];
        if !self.layout_downloads.iter().any(|d| d.path == index)
            || self.layout_downloads.iter().any(read)
            || self.absent_layouts.contains(&id)
        {
            return Ok(());
        }
        self.require_open()?;
        let session = self.session.as_mut().expect("open session");
        let Some(downloaded) = download_node(session, member)? else {
            self.absent_layouts.insert(id);
            self.invalidate_root_schema();
            return self.regrow_schema();
        };
        self.layout_downloads.push(downloaded);
        // A key tree is built in key order.
        self.layout_downloads.sort_by(|a, b| a.path.cmp(&b.path));
        self.catalog.layouts = catalog_layouts(&self.layout_downloads)?;
        *self.own_schema.get_mut() = None;
        self.invalidate_root_schema();
        self.regrow_schema()
    }

    /// [`DataProvider::place_layout_button`] without the refresh: the
    /// layout read now, its revision checked against the expected one, the
    /// button planned as the host plans it
    /// (`om_data::layout_placement::plan_button`), then sent as
    /// [`authoring::PlaceButton`] for Perform Script, or
    /// [`authoring::PlaceButtonStep`] for a single step (#226).
    fn place_button_on_layout(
        &mut self,
        place: &om_data::layout_placement::LayoutButtonPlacement,
    ) -> Result<om_data::layout_placement::PlacedLayoutButton, DataError> {
        use om_data::layout_placement::{self, PlacedLayoutButton, Refusal};
        use om_protocol::authoring::{catalog_revision, status};
        let layout = u64::from(place.layout);
        let member = self.layout_member(layout)?.ok_or_else(|| {
            DataError::with_code(
                i32::from(status::LAYOUT_MISSING),
                format!("The host has no layout {layout}."),
            )
        })?;
        let expected = place.expected;
        if expected != catalog_revision(&member) {
            // A lost answer's button may already be there; only the host
            // can tell, so the request goes with the expected revision.
            if layout_placement::already_placed_button(&member, &place.placement).is_none() {
                return Err(DataError::with_code(
                    layout_placement::CONFLICT,
                    "The layout changed since it was shown; nothing was saved. It is read \
                     again: draw the button on it again.",
                ));
            }
        } else {
            layout_placement::plan_button(&member, &place.placement).map_err(|refusal| {
                match refusal {
                    Refusal::Invalid(reason) => {
                        DataError::with_code(i32::from(status::INVALID), reason)
                    }
                    Refusal::Unsupported(reason) => {
                        DataError::with_code(i32::from(status::UNSUPPORTED), reason)
                    }
                }
            })?;
        }
        let frame = &place.placement.frame;
        let frame = [frame.top, frame.left, frame.bottom, frame.right];
        let label = place.placement.label.clone();
        let placed = match &place.placement.action {
            layout_placement::ButtonAction::PerformScript { script, parameter } => {
                let parameter = parameter.as_ref().map(|calc| {
                    (
                        calc.formatted.clone().unwrap_or_default(),
                        calc.polish.clone().unwrap_or_default(),
                    )
                });
                self.place_button(om_protocol::authoring::PlaceButton {
                    layout,
                    expected,
                    script: *script,
                    frame,
                    label,
                    parameter,
                })
            }
            layout_placement::ButtonAction::Step(step) => {
                self.place_button_step(om_protocol::authoring::PlaceButtonStep {
                    layout,
                    expected,
                    frame,
                    label,
                    id: step.id(),
                    value: step.value,
                    flags: step.flags,
                    reserved: step.reserved,
                    data: step
                        .data
                        .as_ref()
                        .filter(|data| !data.is_empty())
                        .map(|data| data.encode(&[layout_placement::STEP_DATA])),
                })
            }
        };
        let placed = placed.map_err(|error| match error {
            authoring::AuthoringError::Refused {
                status, message, ..
            } => DataError::with_code(i32::from(status), message),
            authoring::AuthoringError::Unavailable(reason) => {
                DataError::with_code(i32::from(status::UNSUPPORTED), reason)
            }
            authoring::AuthoringError::NotSent(reason) => DataError::new(reason),
            authoring::AuthoringError::OutcomeUnknown(reason) => DataError::new(format!(
                "The button was sent but no answer came back ({reason}), so it may or may \
                     not have been placed. The layout is read again from the host."
            )),
        })?;
        Ok(PlacedLayoutButton {
            changed: placed.changed,
            button: placed.field_object,
            revision: placed.revision,
        })
    }

    /// [`DataProvider::place_layout_text`] without the refresh: the layout
    /// read now and the text planned as the host plans it
    /// (`layout_placement::plan_text`), then sent as `om-author`'s
    /// `place_text` sends it.
    fn place_text_on_layout(
        &mut self,
        place: &om_data::layout_placement::LayoutTextPlacement,
    ) -> Result<om_data::layout_placement::PlacedLayoutText, DataError> {
        use om_data::layout_placement::{self, PlacedLayoutText, Refusal};
        use om_protocol::authoring::{catalog_revision, status};
        let layout = u64::from(place.layout);
        let member = self.layout_member(layout)?.ok_or_else(|| {
            DataError::with_code(
                i32::from(status::LAYOUT_MISSING),
                format!("The host has no layout {layout}."),
            )
        })?;
        if place.expected != catalog_revision(&member) {
            // A lost answer's text may already be there; only the host
            // can tell, so the request goes with the expected revision.
            if layout_placement::already_placed_text(&member, &place.text).is_none() {
                return Err(DataError::with_code(
                    layout_placement::CONFLICT,
                    "The layout changed since it was shown; nothing was saved. It is read \
                     again: place the text on it again.",
                ));
            }
        } else {
            layout_placement::plan_text(&member, &place.text).map_err(|refusal| match refusal {
                Refusal::Invalid(reason) => {
                    DataError::with_code(i32::from(status::INVALID), reason)
                }
                Refusal::Unsupported(reason) => {
                    DataError::with_code(i32::from(status::UNSUPPORTED), reason)
                }
            })?;
        }
        let frame = &place.text.frame;
        let placed = self
            .place_text(om_protocol::authoring::PlaceText {
                layout,
                expected: place.expected,
                frame: [frame.top, frame.left, frame.bottom, frame.right],
                text: place.text.text.clone(),
            })
            .map_err(|error| object_edit_error(error, "The text was"))?;
        Ok(PlacedLayoutText {
            changed: placed.changed,
            text_object: placed.field_object,
            revision: placed.revision,
        })
    }

    fn place_portal_on_layout(
        &mut self,
        place: &om_data::layout_placement::LayoutPortalPlacement,
    ) -> Result<om_data::layout_placement::PlacedLayoutPortal, DataError> {
        use om_data::layout_placement::{self, PlacedLayoutPortal, Refusal};
        use om_protocol::authoring::{catalog_revision, status};
        let layout = u64::from(place.layout);
        let member = self.layout_member(layout)?.ok_or_else(|| {
            DataError::with_code(
                i32::from(status::LAYOUT_MISSING),
                format!("The host has no layout {layout}."),
            )
        })?;
        if place.expected != catalog_revision(&member) {
            // A lost answer's portal may already be there; only the host
            // can tell, so the request goes with the expected revision.
            if layout_placement::already_placed_portal(&member, &place.portal).is_none() {
                return Err(DataError::with_code(
                    layout_placement::CONFLICT,
                    "The layout changed since it was shown; nothing was saved. It is read \
                     again: place the portal on it again.",
                ));
            }
        } else {
            // Checked here against the layout as stored; the host plans it
            // again with the layout's theme, under its lock.
            layout_placement::plan_portal(&member, &place.portal, None).map_err(|refusal| {
                match refusal {
                    Refusal::Invalid(reason) => {
                        DataError::with_code(i32::from(status::INVALID), reason)
                    }
                    Refusal::Unsupported(reason) => {
                        DataError::with_code(i32::from(status::UNSUPPORTED), reason)
                    }
                }
            })?;
        }
        let frame = &place.portal.frame;
        let placed = self
            .place_portal(om_protocol::authoring::PlacePortal {
                layout,
                expected: place.expected,
                table: place.table,
                occurrence: place.portal.occurrence,
                frame: [frame.top, frame.left, frame.bottom, frame.right],
                rows: place.portal.rows,
                fields: place.portal.fields.clone(),
            })
            .map_err(|error| object_edit_error(error, "The portal was"))?;
        let portal = placed.field_object;
        Ok(PlacedLayoutPortal {
            changed: placed.changed,
            portal,
            fields: (1..=place.portal.fields.len() as u64)
                .map(|i| portal + i)
                .collect(),
            revision: placed.revision,
        })
    }

    /// Read the layout catalogs this provider downloaded when it opened the
    /// file again, so its layouts are those the host stores now.
    fn refresh_layout_downloads(&mut self) -> Result<(), DataError> {
        self.require_open()?;
        let session = self.session.as_mut().expect("open session");
        let mut fresh = Vec::with_capacity(self.layout_downloads.len());
        for old in &self.layout_downloads {
            if let Some(node) = download_node(session, old.path.clone())? {
                fresh.push(node);
            }
        }
        self.layout_downloads = fresh;
        self.invalidate_root_schema();
        Ok(())
    }

    /// Layout `layout_id`, decoded; refused with 9 when the account's custom
    /// set gives it no access, so nothing is shown or read through it
    /// (session-identity.md 2.2).
    fn decoded_layout(&mut self, layout_id: u32) -> Result<om_layout::Layout, DataError> {
        self.require_open()?;
        if !self.shows_layout(u64::from(layout_id)) {
            return Err(DataError::with_code(
                9,
                "This account's privilege set gives no access to that layout.",
            ));
        }
        self.decoded_layout_for_found_set(layout_id)
    }

    /// Layout `layout_id`, decoded also when the account's custom set
    /// hides it: only for its table, whose found set a script's New Window
    /// or Go to Related Record may move the window through, as FileMaker
    /// Pro 26.0.3 does (#696). Nothing is shown or read through it.
    fn decoded_layout_for_found_set(
        &mut self,
        layout_id: u32,
    ) -> Result<om_layout::Layout, DataError> {
        self.require_open()?;
        let id = u64::from(layout_id);
        // Its member, when only the catalog's index was read for it.
        self.ensure_layout_member(id)?;
        let decode = |tree: &om_hbam::KeyTree<'_>| {
            om_layout::Layout::decode_from_tree(tree, id)
                .map_err(|e| failure("decode server layout", e))?
                .ok_or_else(|| DataError::new(format!("The server has no layout {layout_id}.")))
        };
        // Its theme's member, when only the theme catalog's index was read
        // for it; the tree is built again only when a member was asked for.
        let theme = {
            let tree = download_tree(&self.layout_downloads)?;
            match self.unread_theme(&tree, id) {
                Some(theme) => theme,
                None => return decode(&tree),
            }
        };
        self.read_theme_member(theme)?;
        decode(&download_tree(&self.layout_downloads)?)
    }

    /// The theme layout `layout` names in `tree` (its member's key `0b`),
    /// when the open read only the theme catalog's index `[6, 1]`, as
    /// FileMaker Pro 26 reads it (network-protocol.md 5.5), and the theme's
    /// member `[6, 5, T]` is still to be read: not read yet, not found
    /// absent before, and listed in the index. FileMaker's index lists
    /// every member under `07` (layout-model.md 7.5), so a theme it does not
    /// list is decoded as absent, as the whole catalog would have it, with
    /// no request. An index with no such list leaves the theme to be asked
    /// for once.
    fn unread_theme(&self, tree: &om_hbam::KeyTree<'_>, layout: u64) -> Option<u64> {
        let index = [key_bytes(THEMES), vec![1]];
        if !self.layout_downloads.iter().any(|d| d.path == index) {
            return None;
        }
        let theme = om_layout::theme_reference(tree, layout)?;
        if tree.at(&[THEMES, 5, theme]).is_some() || self.absent_themes.contains(&theme) {
            return None;
        }
        match tree.at(&[THEMES, 1, 7]) {
            Some(entries) if entries.get(theme).is_none() => None,
            _ => Some(theme),
        }
    }

    /// Read theme `theme`'s member `[6, 5, T]` ([`Self::unread_theme`]):
    /// whole, as FileMaker Pro 26 reads it, its styles, metadata and
    /// resources together. When the host has none it is noted, and the
    /// layouts naming it are decoded without a theme, as before.
    fn read_theme_member(&mut self, theme: u64) -> Result<(), DataError> {
        let member = vec![key_bytes(THEMES), vec![5], key_bytes(theme)];
        match download_node(self.session_mut()?, member)? {
            Some(member) => {
                self.layout_downloads.push(member);
                // A key tree is built in key order.
                self.layout_downloads.sort_by(|a, b| a.path.cmp(&b.path));
            }
            None => {
                self.absent_themes.insert(theme);
            }
        }
        Ok(())
    }

    fn table_for_layout(&self, layout: &om_layout::Layout) -> Result<&Table, DataError> {
        let alias = layout.def.as_ref().map_or(0, |d| d.table_alias_key);
        table_for_alias(&self.catalog, alias)
    }

    /// Metadata may belong to another file; resolve the owning route first.
    fn read_table_for_layout(&mut self, layout: &om_layout::Layout) -> Result<Table, DataError> {
        // A layout of another file's table, on demand: that file opens
        // first (calc/demand.rs).
        if let Ok(occurrence) = layout_occurrence(layout) {
            self.grow_related([occurrence])?;
        }
        let schema = match self.calc_schema_cached() {
            Ok(schema) => schema,
            // Local field bindings can still use their downloaded catalog if
            // calculation metadata is unavailable. External occurrences never
            // pass this accessor, so this cannot substitute a root table.
            Err(error) => return self.table_for_layout(layout).cloned().map_err(|_| error),
        };
        let occurrence = layout_occurrence(layout)?;
        let def = schema.table_of(occurrence).ok_or_else(|| {
            DataError::with_code(3, "The layout's table is unavailable in its owning file.")
        })?;
        if schema.table_context(def.id).is_some() {
            self.external.table(def.id)
        } else {
            self.catalog.table(def.id).cloned()
        }
        .ok_or_else(|| DataError::with_code(3, "The layout's table definition is unavailable."))
    }

    /// Read a table on its owning connection and wire id. Imported ids are
    /// client namespaces and must never be sent to the root connection.
    fn with_read_table<T>(
        &mut self,
        table: u64,
        read: impl FnOnce(&mut Session, u64) -> Result<T, DataError>,
    ) -> Result<T, DataError> {
        let schema = self.calc_schema_cached()?;
        if let Some(route) = self.external.route(table) {
            self.external.with_session(route, read)
        } else if schema.table_context(table).is_some() {
            Err(DataError::with_code(
                3,
                "The related file holding this table is unavailable.",
            ))
        } else {
            read(self.session_mut()?, table)
        }
    }

    fn bindings(&mut self, layout: &om_layout::Layout) -> Result<BTreeMap<u64, String>, DataError> {
        let table = self.read_table_for_layout(layout)?;
        let alias = layout.def.as_ref().map_or(0, |d| d.table_alias_key);
        Ok(layout
            .objects
            .objects
            .iter()
            .filter_map(|object| {
                let spec = object.field_spec()?;
                // A related occurrence may point at the same base table but still
                // requires relationship evaluation. Only the layout's own context
                // can use this record page's values.
                if spec.table_key != alias || spec.repetition > 1 {
                    return None;
                }
                Some((object.id, table.field(spec.field_id()?)?.name.clone()))
            })
            .collect())
    }

    /// The fields of related records `layout` shows that Browse may edit
    /// (`RecordCapabilities::related`): for each occurrence one
    /// relationship from the layout's whose fields the layout shows, in a
    /// portal or not, the fields its table's own rules let a user edit
    /// (`RecordPlan::editable_fields`); for a table of another file, its
    /// owner's (`routed_related_fields`). Occurrences further away, and
    /// tables of files that are not open, are left out.
    fn related_capabilities(
        &self,
        file: &om_data::host::CalcSchema,
        layout: &om_layout::Layout,
        tree: &KeyTree<'_>,
    ) -> BTreeMap<u64, BTreeSet<u64>> {
        let Ok(own) = layout_occurrence(layout) else {
            return BTreeMap::new();
        };
        let shown: BTreeSet<u64> = layout
            .objects
            .objects
            .iter()
            .filter_map(|object| occurrence_id(object.field_spec()?.table_key))
            .filter(|occurrence| *occurrence != own)
            .collect();
        let mut tables: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
        let mut related = BTreeMap::new();
        for occurrence in shown {
            if file
                .path(own, occurrence)
                .is_none_or(|path| path.len() != 1)
            {
                continue;
            }
            let Some(table) = file.table_of(occurrence).map(|t| t.id) else {
                continue;
            };
            if shows_related_file(file, occurrence) {
                let fields = tables
                    .entry(table)
                    .or_insert_with(|| self.routed_related_fields(table, file));
                if !fields.is_empty() {
                    related.insert(occurrence, fields.clone());
                }
                continue;
            }
            let fields = tables.entry(table).or_insert_with(|| {
                let Some(catalog) = self.catalog.table(table) else {
                    return BTreeSet::new();
                };
                let names = match om_data::rules::RecordPlan::from_file_schema(catalog, tree, file)
                {
                    Ok(plan) => plan.editable_fields(),
                    // Refused while a related file its write rules read waits
                    // to open: offered as the plan would offer it, as for the
                    // layout's own table; the write opens the file and plans
                    // first (#1139).
                    Err(_) if self.rule_files_pending(file, catalog, tree) => catalog
                        .fields
                        .iter()
                        .filter(|field| om_data::rules::user_editable(field))
                        .map(|field| field.name.clone())
                        .collect(),
                    Err(_) => return BTreeSet::new(),
                };
                // Containers take no typing but Insert Picture and File, on a
                // stored container as `store_table_container` takes it.
                let container = |field: &om_hbam::catalog::Field| {
                    field.data_type == om_hbam::DataType::Container
                        && field.kind == om_hbam::FieldKind::Normal
                        && field
                            .decoded_options()
                            .is_some_and(|o| o.options & 0x1_0000 == 0)
                };
                catalog
                    .fields
                    .iter()
                    .filter(|field| names.contains(&field.name) || container(field))
                    .map(|field| field.id)
                    .collect()
            });
            if !fields.is_empty() {
                related.insert(occurrence, fields.clone());
            }
        }
        related
    }
}

impl NativeProvider {
    /// Start a fresh session without replaying writes, preserving each
    /// window's found set and the fresh login's ephemeral credentials.
    fn reopen_with(
        &mut self,
        open: impl FnOnce(&mut Self) -> Result<(), DataError>,
    ) -> Result<(), DataError> {
        if self.scalar_reconnect_blocked() {
            return Err(DataError::with_code(
                3,
                "Unresolved: the no-data receipt is no longer valid. Drafts are kept.",
            ));
        }
        let found = std::mem::take(&mut self.found);
        let windows = std::mem::take(&mut self.windows);
        let window = self.window;
        // This file's previews keep what the lost session issued (serial
        // numbers, clock and UUID) for their records' saves; a routed one
        // goes with its owner's session (#152).
        let previews = self.previews.take_own();
        // Record sessions of new records never submitted go to the new
        // connection, kept until a reopen succeeds (#152).
        let scalar = self.take_scalar_carry();
        // Root and its related parts must validate together before any op71.
        let carried = if scalar.is_some() {
            Vec::new()
        } else {
            self.take_carried()
        };
        let result = open(self);
        self.found = found;
        self.windows = windows;
        self.window = window;
        self.calc.page = om_data::host::CalcData::default();
        self.previews.put_back(previews);
        if let Err(error) = result {
            self.carried = carried;
            self.scalar_carry = scalar;
            return Err(error);
        }
        // A schema that fails to build now is built again when asked for,
        // without related files: their warnings say so.
        let _ = self.calc_schema_cached();
        self.recreated = self.hold_carried(carried);
        if let Some(scalar) = scalar {
            if let Err(error) = self.recover_scalar_carry(&scalar) {
                self.scalar_carry = Some(scalar);
                return Err(if error.message.starts_with("Unresolved:") {
                    error
                } else {
                    DataError::with_code(3, format!("Unresolved: {} All drafts and issued values are kept; nothing was saved.", error.message))
                });
            }
        }
        // Keep the fresh login's zeroized credentials for later related
        // reads and script opens, just as an ordinary open does. Each
        // target still authenticates with its own account and privileges.
        Ok(())
    }

    /// Page reads update the ordinary observation, never a draft's baseline.
    /// Record `record_id` read through layout `layout_id`, its version
    /// neither seen nor pinned (`DataProvider::record_by_id` does both).
    fn read_record_rows(
        &mut self,
        layout_id: u32,
        record_id: i64,
    ) -> Result<(Table, Vec<om_hbam::record::Record>), DataError> {
        let layout = self.decoded_layout(layout_id)?;
        let table = self.read_table_for_layout(&layout)?;
        let id = u64::try_from(record_id).map_err(|_| DataError::new("Invalid record ID."))?;
        let (data, wire) = self.with_read_table(table.id, |session, wire| {
            Ok((download_records(session, wire, &[id])?, wire))
        })?;
        let tree = download_tree(std::slice::from_ref(&data))?;
        let rows = om_hbam::record::records(&tree, wire)
            .into_iter()
            .filter(|row| row.id == id)
            .collect();
        Ok((table, rows))
    }

    fn saw_version(&mut self, key: (u64, u64), version: u64) {
        self.mod_counts.insert(key, version);
    }

    pub(crate) fn observed_version(&self, key: (u64, u64)) -> Result<Option<u64>, DataError> {
        match self.pinned.get(&(self.window, key.0, key.1)) {
            Some(Some(version)) => Ok(Some(*version)),
            Some(None) => Err(DataError::with_code(
                306,
                "The version of the record shown when editing began is unavailable. \
                 Check the record before saving.",
            )),
            None => Ok(self.mod_counts.get(&key).copied()),
        }
    }

    /// Settling one window's edit does not release another window's draft:
    /// the writing window has seen what it wrote, another window still
    /// answers to what it read.
    pub(crate) fn wrote_version(&mut self, key: (u64, u64), version: Option<u64>) {
        self.saw_own_write(key.0);
        let own = (self.window, key.0, key.1);
        match version {
            Some(version) => {
                self.mod_counts.insert(key, version);
            }
            None => {
                self.mod_counts.remove(&key);
            }
        };
        self.pinned.remove(&own);
    }

    /// Move the values of global fields out of a user's edit of the
    /// layout's record into this session's globals (`set_global`): typing
    /// into a global field on a layout sets the session's value, as Set
    /// Field does (Claris "Global fields"). `true` when there were any.
    fn take_globals(
        &mut self,
        layout_id: u32,
        values: &mut om_data::FieldChanges,
    ) -> Result<bool, DataError> {
        if values.is_empty() {
            return Ok(false);
        }
        // Without a calculation schema no field is known to be global.
        let Ok(schema) = self.calc_schema_cached() else {
            return Ok(false);
        };
        let layout = self.decoded_layout(layout_id)?;
        let table = self.table_for_layout(&layout)?.id;
        let Some(def) = schema.table(table) else {
            return Ok(false);
        };
        let globals: Vec<(String, u64, bool)> = values
            .keys()
            .filter_map(|name| {
                let field = def.fields.values().find(|f| f.name == *name)?;
                field.global.then(|| {
                    (
                        name.clone(),
                        field.id,
                        matches!(
                            field.value_type,
                            om_data::calc::ValueType::Date
                                | om_data::calc::ValueType::Time
                                | om_data::calc::ValueType::Timestamp
                        ),
                    )
                })
            })
            .collect();
        for (name, field, temporal) in &globals {
            let text = values.get(name).cloned().flatten().unwrap_or_default();
            if *temporal {
                self.set_typed_global(table, *field, 1, &text)?;
            } else {
                self.set_global(table, *field, 1, &text)?;
            }
            values.remove(name);
        }
        Ok(!globals.is_empty())
    }

    /// Record `record_id` of the layout's table as stored, with its
    /// calculated fields.
    fn current_record(&mut self, layout_id: u32, record_id: i64) -> Result<Record, DataError> {
        let layout = self.decoded_layout(layout_id)?;
        let table = self.table_for_layout(&layout)?.clone();
        let record = u64::try_from(record_id).map_err(|_| DataError::new("Invalid record ID."))?;
        let node = self
            .read_record(table.id, record)?
            .ok_or_else(|| DataError::with_code(101, "This record no longer exists."))?;
        let stored = records::stored_record(record, &node);
        self.with_calculations(
            layout_id,
            Record {
                id: record_id,
                values: stored.values_for(&table),
            },
        )
    }

    /// A record a write returned, with its calculated fields filled. The
    /// write has succeeded: when they cannot be computed the record is
    /// returned without them rather than reporting a failed save.
    fn with_calculations(&mut self, layout_id: u32, record: Record) -> Result<Record, DataError> {
        let mut filled = record.clone();
        let computed = (|| {
            let layout = self.decoded_layout(layout_id)?;
            let table = self.read_table_for_layout(&layout)?;
            let occurrence = layout_occurrence(&layout)?;
            self.calculate_record(&table, occurrence, &mut filled)
        })();
        Ok(if computed.is_ok() { filled } else { record })
    }
}

impl NativeProvider {
    fn read_page(
        &mut self,
        layout_id: u32,
        offset: u64,
        limit: u32,
        calculated_fields: Option<&[String]>,
    ) -> Result<RecordPage, DataError> {
        self.read_page_summarized(layout_id, offset, limit, calculated_fields)
            .map(|(page, _)| page)
    }

    /// [`Self::read_page`] with the found set's size and sort, read with
    /// the page.
    fn read_page_summarized(
        &mut self,
        layout_id: u32,
        offset: u64,
        limit: u32,
        calculated_fields: Option<&[String]>,
    ) -> Result<(RecordPage, om_data::found::FoundSet), DataError> {
        // What calculations read stays valid across pages until the host
        // says it changed (#1042): kept here, checked below against what
        // this read brings, and given back with the page's own records,
        // record list and found set read fresh. A read that fails keeps
        // nothing.
        let kept = std::mem::take(&mut self.calc.page);
        // A layout the account may not see gives its found set's ids and
        // count, no value (a window a script took there, #696).
        let hidden = !self.shows_layout(u64::from(layout_id));
        let layout = self.decoded_layout_for_found_set(layout_id)?;
        let table = self.read_table_for_layout(&layout)?;
        self.restore_saved_found(&layout, &table);
        // Page through the found set (the table's record list unless a
        // find, omit or sort changed it) and fetch only the page's
        // records, as FileMaker Pro does (query-program.md 7.1,
        // network-protocol.md 5.6.6b).
        let (found, all) = self.found_ids(table.id)?;
        let summary = self.summary_of(table.id, &found, &all);
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let page: Vec<u64> = found
            .iter()
            .copied()
            .skip(start)
            .take(limit as usize)
            .collect();
        if hidden {
            let records = page
                .iter()
                .map(|id| {
                    i64::try_from(*id)
                        .map(|id| Record {
                            id,
                            values: Vec::new(),
                        })
                        .map_err(|_| DataError::new(format!("Record id {id} is out of range.")))
                })
                .collect::<Result<_, _>>()?;
            let page = RecordPage {
                versions: Default::default(),
                fields: Vec::new(),
                records,
                total: found.len() as u64,
            };
            // Nothing read here to check the kept data against: dropped,
            // and windows drop theirs, as on every page read before #1042.
            drop(kept);
            self.calc.generation = self.calc.generation.wrapping_add(1);
            return Ok((page, summary));
        }
        // Records a transaction created are not on the host yet.
        let stored: Vec<u64> = page
            .iter()
            .copied()
            .filter(|id| !matches!(self.transaction_new(table.id, *id), Some(true)))
            .filter(|id| !self.routed_held_new(table.id, *id))
            .collect();
        let (data, wire, served) = self.with_read_table(table.id, |session, wire| {
            let data = download_records(session, wire, &stored)?;
            Ok((data, wire, *session.connection_key()))
        })?;
        let tree = download_tree(std::slice::from_ref(&data))?;
        let page_ids = page;
        let mut rows = om_hbam::record::records(&tree, wire);
        // The op92 reply is in key order; the page is in found-set order.
        rows.sort_by_key(|row| page_ids.iter().position(|id| *id == row.id));
        let generation = self.calc.generation;
        // The page's reply brought the notifications waiting on the
        // connection it was read on; one HostTimeStamp on each other
        // connection of the session brings theirs (none when the related
        // files share it), as before reusing kept scans (#804): the opened
        // file's too when the page is a related file's table. A window may
        // hold data this provider no longer keeps, so this runs whatever
        // was kept here.
        let mut fresh = true;
        if let Some(session) = self
            .session
            .as_mut()
            .filter(|session| *session.connection_key() != served)
        {
            fresh &= session.host_timestamp().is_ok();
        }
        fresh &= self.external.freshen_until(Some(served), &|| false) == Some(true);
        if !fresh {
            self.invalidate_calc_data();
        }
        self.invalidate_calc_notifications();
        let mut changed = false;
        for row in &rows {
            let key = (table.id, row.id);
            let version = records::mod_count(row)?;
            // A record changed since this client last saw it, whether or
            // not a notification said so: what was read from it is stale.
            changed |= self
                .mod_counts
                .get(&key)
                .is_some_and(|seen| *seen != version);
            self.saw_version(key, version);
        }
        if changed {
            self.invalidate_calc_data();
        }
        let kept = if self.calc.generation == generation {
            kept
        } else {
            om_data::host::CalcData::default()
        };
        let all: Vec<u64> = all.iter().collect();
        let mut page = record_page(rows.clone(), &table, found.len() as u64)?;
        let occurrence = layout_occurrence(&layout)?;
        let sets = calc::PageSets {
            all: &all,
            found: &found,
            kept,
        };
        self.calc.page = self.calculate_page(
            occurrence,
            &table,
            &rows,
            sets,
            &mut page,
            calculated_fields,
        )?;
        self.overlay_page(&table, &page_ids, &mut page);
        self.overlay_routed(&table, &page_ids, &mut page);
        Ok((page, summary))
    }
}

impl NativeProvider {
    /// Connect to `connection`'s host at `endpoint`, open its file, log
    /// in and read its catalogs: [`DataProvider::open`] and
    /// [`DataProvider::open_local`] after their checks.
    fn open_at(
        &mut self,
        connection: &Connection,
        endpoint: Endpoint,
        browse: bool,
        via: Via,
    ) -> Result<(), DataError> {
        self.login_refusal = None;
        let local = endpoint.local.is_some();
        // On demand, the layout and theme catalogs' indexes first, and each
        // member when a layout is first read or decoded, as FileMaker Pro 26
        // reads them (#1019, #1129).
        let layout_index = self.related_on_demand && browse;
        let connect = || {
            Session::connect_verified(
                &connection.host,
                connection.port,
                &self.config.client_cert_pem,
                &self.config.client_key_pem,
                &self.config.callback_host,
                &endpoint.machine,
                &endpoint.ca_pem,
            )
            .map_err(|e| {
                if local {
                    failure("open the file", e)
                } else {
                    failure("connect to server", e)
                }
            })
        };
        // On a shared connection, the file open there already for the
        // same account and password is held with it, with no Open or Login
        // of its own; otherwise this handle opens it. Held for another
        // account (or being opened), it opens on a connection of its own:
        // a second Open on one connection would replace it (#1011).
        let admission = refusals::admission(&connection.account, &connection.password);
        let (mut session, mut joined) = match via {
            Via::Own => (connect()?, None),
            Via::Shared(mut handle) => match handle.claim_file(&connection.file, &admission) {
                Ok(om_session::Claim::Joined(login)) => (handle, Some(login)),
                Ok(om_session::Claim::Open) => (handle, None),
                Err(_) => (connect()?, None),
            },
            // Only there: a claim that fails (that connection lost too, the
            // file held there for another account) fails the open, and the
            // file stays lost; it makes no connection of its own (#1011).
            Via::Only(mut handle) => match handle.claim_file(&connection.file, &admission) {
                Ok(om_session::Claim::Joined(login)) => (handle, Some(login)),
                Ok(om_session::Claim::Open) => (handle, None),
                Err(error) => {
                    return Err(failure(
                        "open the file on the reconnected connection",
                        error,
                    ))
                }
            },
        };
        session.set_keepalive(self.keepalive);

        // The file's node `[2]` as read before Login, when this handle
        // logged in (a file joined on a shared connection logs in no more).
        let mut status = None;
        let loaded = (|| {
            let canceled = || {
                if self.close_requests.pending() {
                    Err(DataError::with_code(
                        1,
                        "Opening the database was canceled.",
                    ))
                } else {
                    Ok(())
                }
            };
            canceled()?;
            let login = match joined.take() {
                Some(login) => login,
                None => {
                    session
                .open_file(&connection.host, &connection.file)
                .map_err(|e| {
                    // A client license matters only to a server that asks
                    // for one, never to a file opened from this computer.
                    let hint = if !local && endpoint.machine.install_code.is_empty() {
                        " Add a client license in FileMaker License... if this server requires one."
                    } else {
                        ""
                    };
                    DataError::new(format!("open hosted file: {e}.{hint}"))
                })?;
                    canceled()?;
                    let (creation, file_node) = session
                        .fetch_status()
                        .map_err(|e| failure("read login challenge", e))?;
                    canceled()?;
                    let login = session
                        .login(
                            &connection.account,
                            &connection.password,
                            creation,
                            &login_environment(&connection.host, &connection.file),
                        )
                        .map_err(|e| failure("authenticate to hosted file", e))?;
                    // The file's node `[2]`, read for the challenge, is the
                    // one its calculation schema reads: FileMaker Pro 26
                    // reads it once, before Login (network-protocol.md
                    // 5.6.9b), so it is kept rather than read again.
                    status = node_from_reply(vec![key_bytes(STATUS)], file_node)?;
                    if !login.is_success() && local {
                        let (code, message) = match i32::from(login.error_code) {
                            LOGIN_INACTIVE_ACCOUNT => (
                                LOGIN_INACTIVE_ACCOUNT,
                                format!(
                                    "The account “{}” is inactive in this file.",
                                    connection.account
                                ),
                            ),
                            LOGIN_PASSWORD_CHANGE_DUE => {
                                // Kept for a new password (#157).
                                self.password_change_due = Some((
                                    connection.account.clone(),
                                    zeroize::Zeroizing::new(connection.password.clone()),
                                ));
                                (
                                    LOGIN_PASSWORD_CHANGE_DUE,
                                    format!(
                                        "The password of “{}” must be changed before this file \
                                     opens.",
                                        connection.account
                                    ),
                                )
                            }
                            LOGIN_UNREADABLE_ACCOUNT => (
                                LOGIN_UNREADABLE_ACCOUNT,
                                format!(
                                    "OpenMaker cannot open this file as “{}” yet: it cannot read \
                                 whether this account is active, which kind of account it is, \
                                 or whether its password must be changed.",
                                    connection.account
                                ),
                            ),
                            LOGIN_UNSUPPORTED_PRIVILEGES => (
                                LOGIN_UNSUPPORTED_PRIVILEGES,
                                format!(
                                    "OpenMaker cannot open this file as “{}” yet: its privilege \
                                 set has a limit OpenMaker does not enforce yet, so the \
                                 account would get more access than its set gives it.",
                                    connection.account
                                ),
                            ),
                            _ => (
                                LOGIN_REFUSED,
                                "The account name and password you entered cannot be used to \
                             access this file."
                                    .to_owned(),
                            ),
                        };
                        let error = DataError::with_code(code, message);
                        self.login_refusal = Some(error.clone());
                        return Err(error);
                    }
                    if !login.is_success() {
                        let error = DataError::with_code(
                    match login.error_code {
                        0xd0 | 0x20d0 => 3,
                        om_server::LOGIN_TOO_MANY_ATTEMPTS => LOGIN_TOO_MANY_ATTEMPTS,
                        _ => 212,
                    },
                    match login.error_code {
                    0xd0 => "The server requires a password change; this native client does not \
                             support that flow yet."
                        .into(),
                    0x20d0 => "The server requested external authentication; this native client \
                               supports local FileMaker accounts only."
                        .into(),
                    om_server::LOGIN_TOO_MANY_ATTEMPTS => "The server refused the login after too \
                             many failed attempts. Wait a few minutes, then try again."
                        .into(),
                    code => format!("The server rejected login (error 0x{code:04x})."),
                });
                        self.login_refusal = Some(error.clone());
                        return Err(error);
                    }
                    canceled()?;
                    session
                        .request_notifications()
                        .map_err(|e| failure("subscribe to file changes", e))?;
                    // Another handle on the connection opening the file for the
                    // same account and password holds it with this one.
                    session
                        .admit_file(admission.clone())
                        .map_err(|e| failure("share the file", e))?;
                    login
                }
            };
            let privileges = privileges::read_current_account(
                &mut session,
                &login.result_key,
                &self.close_requests,
            );
            canceled()?;
            let mut schema = if browse {
                vec![download(&mut session, &[3])?]
            } else {
                download_node(&mut session, vec![key_bytes(3)])?
                    .into_iter()
                    .collect()
            };
            let base = if schema.is_empty() {
                Catalog::default()
            } else {
                Catalog::from_tree(&download_tree(&schema)?)
                    .map_err(|e| failure("decode server schema", e))?
            };
            for table in &base.tables {
                canceled()?;
                schema.push(download(&mut session, &[table.id, 3])?);
            }
            let mut catalog = if schema.is_empty() {
                Catalog::default()
            } else {
                Catalog::from_tree(&download_tree(&schema)?)
                    .map_err(|e| failure("decode server fields", e))?
            };
            canceled()?;
            let mut layouts = Vec::new();
            // Metadata authoring permits absent UI catalogs. Missing and
            // malformed remain distinct; decoding errors still fail open.
            // The image library [31] is optional: a blank file FileMaker
            // Pro 26 makes has none (file-format.md 8), and a layout
            // with no images reads none from it (library-images.md).
            for key in [4, THEMES, 25, 31] {
                if (key == 4 || key == THEMES) && layout_index {
                    match download_node(&mut session, vec![key_bytes(key), vec![1]])? {
                        Some(index) => layouts.push(index),
                        // A catalog with no index, which FileMaker does not
                        // make: read whole, as before.
                        None => layouts.push(download(&mut session, &[key])?),
                    }
                } else if browse && key != 31 {
                    layouts.push(download(&mut session, &[key])?);
                } else if let Some(node) = download_node(&mut session, vec![key_bytes(key)])? {
                    layouts.push(node);
                }
            }
            catalog.layouts = catalog_layouts(&layouts)?;
            if browse && (catalog.tables.is_empty() || catalog.layouts.is_empty()) {
                return Err(DataError::new(
                    "The server returned no accessible tables or layouts for this account.",
                ));
            }
            let account_info = self
                .local
                .as_ref()
                .map(|host| host.authenticated_account(&login.result_key))
                .unwrap_or_else(|| om_data::host::AccountInfo {
                    // Hosted Root uses its own authenticated set reply,
                    // as each hosted related owner does. Local hosts keep
                    // their authoritative account context above.
                    set_access: privileges.access.clone(),
                    ..Default::default()
                });
            Ok((catalog, schema, layouts, privileges, account_info))
        })();
        match loaded {
            Ok((catalog, schema, layouts, privileges, account_info)) => {
                self.status_download = status;
                // A hosted file's peers open on its host and port; a local
                // file's beside it, each through a host of its own that the
                // family shares (local.rs), never on a server.
                self.peer_auth = (!local).then(|| PeerAuth {
                    host: connection.host.clone(),
                    port: connection.port,
                    account: connection.account.clone(),
                    password: Some(zeroize::Zeroizing::new(connection.password.clone())),
                });
                self.local_auth = local.then(|| LocalAuth {
                    account: connection.account.clone(),
                    password: Some(zeroize::Zeroizing::new(connection.password.clone())),
                });
                self.privileges = privileges;
                self.account_info = account_info;
                self.external = match &endpoint.local {
                    Some(path) => external::ExternalFiles::for_local_file(
                        path,
                        connection,
                        self.keepalive,
                        self.local_hosts.clone(),
                    ),
                    None => external::ExternalFiles::for_connection(connection, self.keepalive),
                }
                .remembering(self.login_refusals.clone());
                self.external.set_on_demand(self.related_on_demand);
                self.external.set_retry_after(self.related_retry_after);
                self.session = Some(session);
                self.catalog = catalog;
                self.schema_downloads = schema;
                *self.own_schema.get_mut() = None;
                self.layout_downloads = layouts;
                self.absent_layouts.clear();
                self.absent_themes.clear();
                self.account_name = connection.account.clone();
                // `Get ( HostApplicationVersion )` is empty for a file that
                // is not hosted.
                self.host_version = if local {
                    String::new()
                } else {
                    self.session
                        .as_ref()
                        .map(|s| s.identity().version_string.clone())
                        .unwrap_or_default()
                };
                self.found.clear();
                self.windows.clear();
                self.window = 1;
                self.reset_saved_found();
                self.calc = calc::CalcState {
                    file_name: om_data::file_refs::file_stem(&connection.file).to_owned(),
                    // A local file's host is this computer.
                    host_name: if local {
                        om_data::host::machine::Machine::current().host_name.clone()
                    } else {
                        connection.host.clone()
                    },
                    file_path: endpoint
                        .local
                        .as_deref()
                        .map(om_data::file_refs::local_file_path),
                    // Read with the layout catalogs (#1098).
                    fonts: std::cell::RefCell::new(Some(file_fonts(&self.layout_downloads)?)),
                    ..calc::CalcState::default()
                };
                self.register_desktop_file(None);
                Ok(())
            }
            Err(error) => {
                let _ = session.disconnect();
                Err(error)
            }
        }
    }
}

impl NativeProvider {
    /// Authenticate and read catalogs without desktop Browse prerequisites.
    pub fn open_metadata(&mut self, connection: &Connection) -> Result<(), DataError> {
        self.open_catalog(connection, false)
    }

    fn open_catalog(&mut self, connection: &Connection, browse: bool) -> Result<(), DataError> {
        self.open_catalog_on(connection, browse, Via::Own)
    }

    /// [`Self::open_catalog`], on the connection `via` says: one of its
    /// own, or a handle on one to the file's host already made
    /// ([`Session::share`]), with no host lookup, TLS handshake or Connect
    /// of its own, as FileMaker Pro opens another file of a host in the
    /// session it has there (#1011, network-protocol.md 5.5).
    fn open_catalog_on(
        &mut self,
        connection: &Connection,
        browse: bool,
        via: Via,
    ) -> Result<(), DataError> {
        self.close();
        if connection.host.trim().is_empty() || connection.file.trim().is_empty() {
            return Err(DataError::new(
                "A server and hosted file name are required.",
            ));
        }
        let endpoint = Endpoint {
            ca_pem: self.config.additional_ca_pem.clone(),
            machine: self.config.machine.clone(),
            local: None,
        };
        self.open_at(connection, endpoint, browse, via)
    }

    /// [`DataProvider::open_peer`] of a file opened from this computer:
    /// `file`, a plain name, beside it (`local_peer_path`), in a provider
    /// of its own in this one's family (local.rs), logged in to with the
    /// account and password this file was opened with and admitted by that
    /// file's own accounts, as File > Open admits it. A file the family has
    /// open already (a related file) shares its host; nothing goes to a
    /// server and no license is read.
    fn open_local_peer(
        &mut self,
        file: &str,
        credentials: Option<(&str, &str)>,
    ) -> Result<Box<dyn DataProvider>, DataError> {
        if self.close_requests.pending() {
            return Err(DataError::with_code(
                1,
                "Opening the database was canceled.",
            ));
        }
        let Some(path) = self.local_peer_path(file) else {
            return Err(DataError::with_code(
                3,
                format!(
                    "“{file}” names no file beside this one: OpenMaker opens only files in \
                     the same folder from a file on this computer."
                ),
            ));
        };
        let (account, password) = match credentials {
            Some((account, password)) => (
                account.to_owned(),
                zeroize::Zeroizing::new(password.to_owned()),
            ),
            None => {
                let auth = self
                    .local_auth
                    .as_ref()
                    .ok_or_else(|| DataError::with_code(100, "No file is open."))?;
                let Some(password) = &auth.password else {
                    return Err(DataError::with_code(802,
                        "This file was reconnected without keeping its password, so it cannot open another file. Close it and open it again first."));
                };
                (
                    auth.account.clone(),
                    zeroize::Zeroizing::new(password.to_string()),
                )
            }
        };
        // A file that refused these credentials in this session is not
        // sent them again (refusals.rs).
        let key = refusals::FileKey::local(&path);
        let refusals = self.login_refusals.clone();
        if let Some(error) = refusals
            .lock()
            .expect("login refusals")
            .refused(&key, &account, &password)
        {
            return Err(error);
        }
        let credentials = Some((account.clone(), password.clone()));
        let mut peer = NativeProvider::new(self.config.clone())
            .with_keepalive(self.keepalive)
            .with_related_on_demand(self.related_on_demand);
        peer.close_requests = self.close_requests.clone();
        peer.desktop = self.desktop.clone();
        peer.local_hosts = self.local_hosts.clone();
        peer.login_refusals = refusals.clone();
        let opened = peer.open_local_session(&om_data::LocalFile { path, credentials });
        if let Some(error) = &peer.login_refusal {
            refusals
                .lock()
                .expect("login refusals")
                .record(key, &account, &password, error);
        }
        let opened = opened?;
        if !matches!(opened, om_data::LocalOpen::Opened { .. }) {
            return Err(DataError::with_code(
                212,
                "The file did not accept this file's account.",
            ));
        }
        if self.close_requests.pending() || self.connection_lost().is_some() {
            peer.close();
            return Err(DataError::with_code(
                1,
                "Opening the database was canceled.",
            ));
        }
        Ok(Box::new(peer))
    }

    /// [`DataProvider::open_peer`] with `credentials`, or else the account
    /// and password this file was opened with ([`DataProvider::open_peer_as`]).
    fn open_peer_with(
        &mut self,
        file: &str,
        credentials: Option<(&str, &str)>,
    ) -> Result<Box<dyn DataProvider>, DataError> {
        self.require_open()?;
        if self.local.is_some() {
            return self.open_local_peer(file, credentials);
        }
        if self.close_requests.pending() {
            return Err(DataError::with_code(
                1,
                "Opening the database was canceled.",
            ));
        }
        let auth = self
            .peer_auth
            .as_ref()
            .ok_or_else(|| DataError::with_code(100, "No open host session."))?;
        // The file's host and port: this file's, or another named as
        // `fmnet:/host:port/Name` (#976), which is sent an account that
        // host admitted, else this file's once
        // ([`Self::other_host_account`]), and then only one typed for it.
        // No empty password is sent [I: FileMaker Pro sends one next,
        // network-protocol.md 5.5; what it stands for is not established].
        let (host, port, file, elsewhere) = peer_target(auth, file);
        let there = match (elsewhere, credentials) {
            (true, None) => Some(
                self.other_host_account(&host, port, &file)
                    .ok_or_else(|| another_host(&host, port, &file))?,
            ),
            _ => None,
        };
        let credentials = credentials.or(there
            .as_ref()
            .map(|(account, password)| (account.as_str(), password.as_str())));
        let (account, password) = match credentials {
            Some((account, password)) => (
                account.to_owned(),
                zeroize::Zeroizing::new(password.to_owned()),
            ),
            None => {
                let Some(password) = &auth.password else {
                    return Err(DataError::with_code(802,
                        "This file was reconnected without keeping its password, so it cannot open another file. Close it and open it again first."));
                };
                (
                    auth.account.clone(),
                    zeroize::Zeroizing::new(password.to_string()),
                )
            }
        };
        // A file that refused these credentials in this session is not
        // sent them again (refusals.rs).
        let key = refusals::FileKey::hosted(&host, port, &file);
        let refusals = self.login_refusals.clone();
        if let Some(error) = refusals
            .lock()
            .expect("login refusals")
            .refused(&key, &account, &password)
        {
            return Err(error);
        }
        let mut peer = NativeProvider::new(self.config.clone())
            .with_keepalive(self.keepalive)
            .with_related_on_demand(self.related_on_demand);
        peer.close_requests = self.close_requests.clone();
        peer.desktop = self.desktop.clone();
        peer.login_refusals = refusals.clone();
        // In the session this client has on the file's host, as FileMaker
        // Pro opens a host's files on one connection (#1011,
        // network-protocol.md 5.5): this file's, or on another host a
        // related file's there. A lost one is not used: the file connects.
        let shared = if elsewhere {
            self.external.session_on(&host, port)
        } else {
            self.session.as_ref().map(Session::share)
        }
        .filter(|session| session.connection_lost().is_none());
        // The same TLS identity and machine license, on the file's host and
        // port: the current one, or another the account was typed for.
        let mut connection = Connection {
            host,
            port,
            account,
            password: password.to_string(),
            file,
        };
        let result = peer.open_catalog_on(&connection, true, shared.map_or(Via::Own, Via::Shared));
        if let Some(error) = &peer.login_refusal {
            refusals.lock().expect("login refusals").record(
                key,
                &connection.account,
                &connection.password,
                error,
            );
        }
        // Another host admitted the account: its other files are sent it
        // first, a script's opens and related files alike
        // ([`Self::other_host_account`]).
        if elsewhere && result.is_ok() {
            self.external.admitted_on(
                &connection.host,
                connection.port,
                &connection.account,
                &connection.password,
            );
        }
        use zeroize::Zeroize;
        connection.password.zeroize();
        result?;
        if self.close_requests.pending() || self.connection_lost().is_some() {
            peer.close();
            return Err(DataError::with_code(
                1,
                "Opening the database was canceled.",
            ));
        }
        Ok(Box::new(peer))
    }

    /// [`DataProvider::open_local`] within the current session, keeping
    /// its memory of refused Logins (refusals.rs): a Reconnect and a peer.
    fn open_local_session(
        &mut self,
        file: &om_data::LocalFile,
    ) -> Result<om_data::LocalOpen, DataError> {
        // A host waiting for this file's account is kept; any other open
        // file closes first.
        let waiting = self
            .local
            .take()
            .filter(|host| self.session.is_none() && same_file(host.path(), &file.path));
        self.close();
        // The family's host of the file when one of its sessions has it
        // open (a related file, a peer), else a new one (local.rs).
        let host = match waiting {
            Some(host) => host,
            None => self
                .local_hosts
                .acquire(&file.path, &self.config.client_cert_pem)?,
        };
        // FileMaker Pro opens some files without asking, by File Options
        // OpenMaker does not read: it always asks, suggesting the account
        // the file names (not a sign that it would open without asking).
        let Some((account, password)) = &file.credentials else {
            let suggested = host.suggested_account().unwrap_or_default();
            // File Options' "Log in using" (local-files.md 3.1): FileMaker
            // Pro opens the file with it without asking, and asks when it
            // is refused or when the user holds Option or Shift.
            let stored = (!self.ask_for_account)
                .then(|| host.stored_login())
                .flatten();
            self.local = Some(host);
            if let Some((account, password)) = stored {
                let stored = om_data::LocalFile {
                    path: file.path.clone(),
                    credentials: Some((account, password)),
                };
                match self.open_local_session(&stored) {
                    Ok(opened) => return Ok(opened),
                    // Refused: the file waits for an account, as after a
                    // typed one; the stored password is not kept for a
                    // password change (211), the typed account is.
                    Err(error) if error.code.is_some_and(local_refusal) => {
                        self.password_change_due = None;
                    }
                    Err(error) => return Err(error),
                }
            }
            return Ok(om_data::LocalOpen::NeedsLogin { account: suggested });
        };
        let mut connection = Connection {
            host: "127.0.0.1".into(),
            port: host.port(),
            file: host.file_name().to_owned(),
            account: account.clone(),
            password: password.to_string(),
        };
        let read_only = host.read_only().map(|reason| reason.to_string());
        let endpoint = Endpoint {
            ca_pem: host.certificate_pem().to_vec(),
            // No client license goes to a file OpenMaker opens itself.
            machine: MachineInfo {
                install_code: String::new(),
                ..self.config.machine.clone()
            },
            local: Some(host.path().to_path_buf()),
        };
        // The lease stays with the provider while the file is open, or
        // while it asks for an account.
        self.local = Some(host);
        let opened = self.open_at(&connection, endpoint, true, Via::Own);
        use zeroize::Zeroize;
        connection.password.zeroize();
        match opened {
            Ok(()) => Ok(om_data::LocalOpen::Opened { read_only }),
            // A refused account keeps the file waiting for another.
            Err(error) if error.code.is_some_and(local_refusal) => Err(error),
            Err(error) => {
                self.release_local();
                Err(error)
            }
        }
    }
}

impl NativeProvider {
    /// Delete Record through layout `layout_id`, by the user or (`script`)
    /// by a script step ([`DataProvider::delete_record_in_script`]).
    fn delete_layout_record(
        &mut self,
        layout_id: u32,
        record_id: i64,
        script: bool,
    ) -> Result<(), DataError> {
        self.refuse_beside_record_session()?;
        self.refuse_while_unsettled()?;
        self.require_writable()?;
        self.require_layout_records(layout_id)?;
        let layout = self.decoded_layout(layout_id)?;
        let table = self.read_table_for_layout(&layout)?.id;
        if self.external.route(table).is_none() {
            self.require_delete(table, record_id)?;
        }
        if self.external.route(table).is_some() {
            // Keep the caller's layout/scripts and client table namespace.
            // The owner supplies the complete ordinary deletion engine,
            // including verified absence before container-member release.
            self.invalidate_calc_data();
            if script && self.in_transaction() {
                // Held in the owner's part of the script's transaction, as
                // a portal row of that file is (record-writes.md, "A
                // script's transaction across files"): deleted when the
                // transaction commits, kept when it is reverted.
                return self.delete_routed_related(table, record_id);
            }
            // The user's own deletion while a script's transaction is open
            // is refused here (3), as the user's edits are: no step of the
            // transaction made it.
            return self.with_existing_record_owner(table, record_id, |owner, wire| {
                owner.delete_related_record(wire, record_id)
            });
        }
        self.delete_related_record(table, record_id)
    }
}

impl DataProvider for NativeProvider {
    fn supports_local_files(&self) -> bool {
        true
    }

    fn open_local_asking(
        &mut self,
        path: &std::path::Path,
    ) -> Result<om_data::LocalOpen, DataError> {
        // A new session: no file has refused its credentials yet.
        self.login_refusals = Default::default();
        self.password_change_due = None;
        self.ask_for_account = true;
        let opened = self.open_local_session(&om_data::LocalFile {
            path: path.to_path_buf(),
            credentials: None,
        });
        self.ask_for_account = false;
        opened
    }

    fn open_local(&mut self, file: &om_data::LocalFile) -> Result<om_data::LocalOpen, DataError> {
        // A new session: no file has refused its credentials yet.
        self.login_refusals = Default::default();
        self.password_change_due = None;
        self.open_local_session(file)
    }

    fn change_due_password(&mut self, new: &str) -> Result<om_data::LocalOpen, DataError> {
        let Some((account, old)) = self.password_change_due.take() else {
            return Err(DataError::with_code(
                3,
                "No account of this file is waiting for a new password.",
            ));
        };
        let Some(host) = self.local.as_ref() else {
            return Err(DataError::with_code(
                100,
                "No file is waiting for an account.",
            ));
        };
        if let Err(refusal) = host.change_password(&account, &old, new) {
            use om_server::hosting::PasswordChangeError as Refusal;
            let error = password_change_error(&refusal, &account);
            // The same account may try another new password.
            if matches!(
                refusal,
                Refusal::NotAllowed
                    | Refusal::TooShort(_)
                    | Refusal::Unchanged
                    | Refusal::TooLong
                    | Refusal::Unwritten(_)
            ) {
                self.password_change_due = Some((account, old));
            }
            return Err(error);
        }
        let path = host.path().to_path_buf();
        self.open_local_session(&om_data::LocalFile {
            path,
            credentials: Some((account, zeroize::Zeroizing::new(new.to_owned()))),
        })
    }

    fn change_password(&mut self, old: &str, new: &str) -> Result<(), DataError> {
        self.require_open()?;
        let Some(host) = self.local.as_ref() else {
            return Err(DataError::with_code(
                3,
                "OpenMaker changes a password only in a file on this computer.",
            ));
        };
        host.change_password(&self.account_name, old, new)
            .map_err(|refusal| password_change_error(&refusal, &self.account_name))?;
        // A pair refused before the change (a script's sign-in to this file
        // from another) is the file's password now.
        let key = refusals::FileKey::local(host.path());
        self.login_refusals
            .lock()
            .expect("login refusals")
            .admitted(&key, &self.account_name, new);
        // The session's password from now on, for the files beside it its
        // scripts open [I: FileMaker's own use of it is unmeasured].
        if let Some(auth) = &mut self.local_auth {
            if auth.password.is_some() {
                auth.password = Some(zeroize::Zeroizing::new(new.to_owned()));
            }
        }
        Ok(())
    }

    fn supports_client_license(&self) -> bool {
        true
    }

    fn set_client_license(&mut self, license: Option<&str>) -> Result<(), DataError> {
        self.config.machine.install_code = license.unwrap_or_default().to_owned();
        Ok(())
    }

    fn can_write_records(&self) -> bool {
        // What can be written depends on each table's field rules; the GUI
        // asks per layout (`record_capabilities`).
        false
    }

    fn no_access(&mut self, layout_id: u32, page: &RecordPage) -> om_data::NoAccess {
        use om_data::host::privilege_set::AccessLevel;
        use om_data::host::security::{RecordAction, RecordView};
        let mut hidden = om_data::NoAccess::default();
        let Some(access) = self.account_info.set_access.clone() else {
            return hidden;
        };
        hidden.judged = true;
        // Records through the layout view only answer 1 (#705). Without
        // access, the window masks the layout's objects (#707), and
        // `Get ( RecordAccess )` keeps the table's answer, as FileMaker
        // Pro 26.0.3 gives it (2 where the table allows editing).
        let through = access.layout_records(u64::from(layout_id));
        hidden.layout = through == AccessLevel::No;
        let Ok(layout) = self.decoded_layout(layout_id) else {
            return hidden;
        };
        let Ok(table) = self.table_for_layout(&layout).cloned() else {
            return hidden;
        };
        let key = om_hbam::slot::uint_segment(table.id);
        // The page's fields by schema name, with their ids in the table.
        let ids: Vec<Option<u64>> = page
            .fields
            .iter()
            .map(|field| {
                table
                    .fields
                    .iter()
                    .find(|f| f.name == field.name)
                    .map(|f| f.id)
            })
            .collect();
        for (field, id) in page.fields.iter().zip(&ids) {
            if id.is_some_and(|id| access.hides_field(&key, &om_hbam::slot::uint_segment(id))) {
                hidden.fields.insert(field.name.clone());
            }
        }
        let row = |record: &Record| -> BTreeMap<u64, String> {
            ids.iter()
                .zip(&record.values)
                .filter_map(|(id, value)| Some(((*id)?, value.clone()?)))
                .collect()
        };
        let edit_limit = access.limit(&key, RecordAction::Edit).map(<[u8]>::to_vec);
        let edits = through != AccessLevel::ViewOnly && access.may_ever(&key, RecordAction::Edit);
        for record in &page.records {
            let values = row(record);
            // A record the formula is not true on, as this provider holds it
            // (withheld, its values are empty; host and desktop agree on an
            // empty record).
            let viewable = match access.record_view(&key) {
                RecordView::All => true,
                RecordView::Never => false,
                RecordView::Where(code) => {
                    let code = code.to_vec();
                    self.formula_allows(table.id, &code, &values)
                        .unwrap_or(false)
                }
            };
            if !viewable {
                hidden.records.insert(record.id);
                continue;
            }
            let editable = edits
                && edit_limit.as_ref().is_none_or(|code| {
                    self.formula_allows(table.id, code, &values)
                        .unwrap_or(false)
                });
            if !editable {
                hidden.view_only.insert(record.id);
            }
        }
        hidden
    }

    fn record_access(&mut self, layout_id: u32, record_id: i64) -> Option<i64> {
        self.account_info.set_access.as_ref()?;
        // Not `record_by_id`: a pending edit's conflict baseline (the
        // version pinned when editing began) must not move for a `Get`.
        let (table, rows) = self.read_record_rows(layout_id, record_id).ok()?;
        let count = rows.len() as u64;
        let page = record_page(rows, &table, count).ok()?;
        self.no_access(layout_id, &page).record_access(record_id)
    }

    fn record_capabilities(
        &mut self,
        layout_id: u32,
    ) -> Result<om_data::RecordCapabilities, DataError> {
        let none = || om_data::RecordCapabilities {
            editable_fields: Some(BTreeSet::new()),
            ..Default::default()
        };
        // A window whose save may be partly applied takes no edits until it
        // is settled (#1006).
        if self.read_only() || self.refuse_while_unsettled().is_err() {
            return Ok(none());
        }
        // Records through a layout view only or without access, or under a
        // set that edits no record, take no create, edit or delete on it;
        // Browse still lets the user enter the fields it would offer, and
        // refuses what is typed (`refused`).
        let through =
            self.layout_records(layout_id) == om_data::host::privilege_set::AccessLevel::Modifiable;
        let denied = !self.records_allowed() || !through;
        let layout = self.decoded_layout(layout_id)?;
        // The related files it reads open first, so their fields' rules
        // count (#983): the window asks before it draws the layout.
        let file = self.calc_schema_for_layout(&layout)?;
        if shows_related_file(&file, layout_occurrence(&layout)?) {
            if denied {
                return Ok(none());
            }
            let table = self.read_table_for_layout(&layout)?;
            let route = self.external.route(table.id).ok_or_else(|| {
                DataError::with_code(3, "The related file holding this table is unavailable.")
            })?;
            // Planned over the owner's routed plan (#187), as its write is,
            // opening nothing; deferred while a file its rules read waits
            // (`routed_edits`, #1160).
            let editable = match self.routed_edits(route, &file)? {
                related_owner::RoutedEdits::ReadOnly => {
                    return Ok(om_data::RecordCapabilities {
                        editable_fields: Some(BTreeSet::new()),
                        ..Default::default()
                    });
                }
                // Only the fields a routed write takes (`check_scalar_cells`):
                // a repeating field the owner's plan offers is not (#185).
                related_owner::RoutedEdits::Offered(names, table) => Some(
                    names
                        .into_iter()
                        .filter(|name| {
                            table.fields.iter().any(|field| {
                                field.name == *name
                                    && Self::check_scalar_cells(&table, &[(field.id, None)].into())
                                        .is_ok()
                            })
                        })
                        .collect(),
                ),
                related_owner::RoutedEdits::Refused => None,
            };
            return Ok(om_data::RecordCapabilities {
                prepare_edit: false,
                // Creation and edits share the owner's field-entry plan.
                create: editable.is_some(),
                update: editable.is_some(),
                // Deletion uses the owning lock/commit/readback engine;
                // it does not evaluate the table's field-entry rules.
                delete: true,
                editable_fields: Some(editable.unwrap_or_default()),
                // Related records of a layout of another file's table
                // are not edited in Browse yet.
                related: BTreeMap::new(),
                refused: BTreeMap::new(),
                globals: BTreeSet::new(),
            });
        }
        let table = self.table_for_layout(&layout)?;
        let tree = download_tree(&self.schema_downloads)?;
        // Creating and editing compute the client's field rules; deleting
        // needs none (write-path.md 7). The window asks before it draws the
        // layout, so the rules are planned over the schema as the layout
        // left it: a related file only the table's write rules read does not
        // open for this. A plan refused while such a file waits is offered
        // from the fields' definitions (`user_editable`), as the plan would
        // offer them: the write opens the file and plans before anything is
        // sent (`plan_table`), and a refusal then fails it, the window's
        // drafts kept (#1139).
        let planned = om_data::rules::RecordPlan::from_file_schema(table, &tree, &file);
        let deferred = planned.is_err() && self.rule_files_pending(&file, table, &tree);
        let plan: Option<BTreeSet<String>> = match &planned {
            Ok(plan) => Some(plan.editable_fields()),
            Err(_) if deferred => Some(
                table
                    .fields
                    .iter()
                    .filter(|field| om_data::rules::user_editable(field))
                    .map(|field| field.name.clone())
                    .collect(),
            ),
            Err(_) => None,
        };
        // Text globals and plain number/date/time/timestamp globals take the session's
        // value (`take_globals`). Non-text globals with validation
        // stay display-only: their field-exit checks are not implemented.
        let globals = file.table(table.id).into_iter().flat_map(|t| {
            t.fields
                .values()
                .filter(|f| {
                    let plain = f.class == om_data::host::FieldClass::Data
                        && matches!(
                            f.value_type,
                            om_data::calc::ValueType::Number
                                | om_data::calc::ValueType::Date
                                | om_data::calc::ValueType::Time
                                | om_data::calc::ValueType::Timestamp
                        )
                        && table
                            .field(f.id)
                            .and_then(|field| field.decoded_options())
                            .is_some_and(|options| {
                                options.validation_options == 0
                                    && !options.auto_entry_editing_prohibited()
                            });
                    f.global
                        && f.repetitions <= 255
                        && (f.value_type == om_data::calc::ValueType::Text || plain)
                })
                .map(|f| f.name.clone())
        });
        let offered = plan.clone().map(|mut fields| {
            fields.extend(globals);
            fields
        });
        // The account's privilege set limits each action on the table, and
        // Browse lets the user change no field it gives view-only access.
        use om_data::host::security::RecordAction;
        use om_data::EditRefusal;
        let may = |action| !denied && self.require_action(table.id, action).is_ok();
        let modifies = |name: &String| {
            let access = self.account_info.set_access.as_ref();
            table.field_by_name(name).is_none_or(|field| {
                access.is_none_or(|access| {
                    access.modifies(
                        &om_hbam::slot::uint_segment(table.id),
                        &om_hbam::slot::uint_segment(field.id),
                    )
                })
            })
        };
        let update = plan.is_some() && may(RecordAction::Edit);
        let refused = offered
            .iter()
            .flatten()
            .filter_map(|name| {
                let refusal = match (update, modifies(name)) {
                    (false, _) => EditRefusal::Privileges,
                    (true, false) => EditRefusal::ViewOnlyField,
                    (true, true) => return None,
                };
                Some((name.clone(), refusal))
            })
            .collect();
        let editable_fields = match denied {
            true => Some(BTreeSet::new()),
            false => offered.map(|fields| fields.into_iter().filter(modifies).collect()),
        };
        let mut related = if denied {
            BTreeMap::new()
        } else {
            self.related_capabilities(&file, &layout, &tree)
        };
        if let Some(fields) = &editable_fields {
            related.insert(
                layout_occurrence(&layout)?,
                table
                    .fields
                    .iter()
                    .filter(|field| fields.contains(&field.name))
                    .map(|field| field.id)
                    .collect(),
            );
        }
        Ok(om_data::RecordCapabilities {
            prepare_edit: table.fields.iter().any(|f| {
                f.kind == om_hbam::FieldKind::Normal && f.data_type == om_hbam::DataType::Container
            }),
            create: plan.is_some() && may(RecordAction::Create),
            update,
            delete: may(RecordAction::Delete),
            editable_fields,
            related,
            refused,
            globals: file
                .table(table.id)
                .into_iter()
                .flat_map(|t| t.fields.values().filter(|f| f.global))
                .map(|f| f.name.clone())
                .collect(),
        })
    }

    fn create_record(
        &mut self,
        layout_id: u32,
        mut values: om_data::FieldChanges,
    ) -> Result<Record, DataError> {
        self.require_writable()?;
        self.refuse_beside_record_session()?;
        self.refuse_while_unsettled()?;
        self.require_layout_records(layout_id)?;
        // Even a lost write reply may have changed the host. Draft and
        // transaction outcome handling remains with the existing write path.
        self.invalidate_calc_data();
        let layout = self.decoded_layout(layout_id)?;
        let table = self.read_table_for_layout(&layout)?;
        self.check_asset_write(
            &om_data::ScriptWrite {
                layout: layout_id,
                record: None,
                fields: values.clone(),
                ..Default::default()
            },
            table.id,
        )?;
        // A record made before the window read the table joins the saved
        // found set it starts on (found.rs).
        self.restore_saved_found(&layout, &table);
        if self.external.route(table.id).is_none() {
            self.require_action(table.id, om_data::host::security::RecordAction::Create)?;
        }
        if self.external.route(table.id).is_some() {
            // The owner issues the id and stores every value; the caller's
            // window found set takes the new record (`with_new_record_owner`).
            let record = self.with_new_record_owner(table.id, |owner, wire| {
                let (table, plan) = owner.table_plan(wire)?;
                let changes = Self::writable_values(&table, &plan, &values)?;
                Self::check_scalar_cells(&table, &changes)?;
                // The window's layout is this file's, not the owner's: its
                // number is not answered there, and the owner's own session
                // says whose set is current.
                let context = owner.write_context(&plan, None, records::CurrentSet::Account)?;
                owner.insert_table_record(&table, &plan, &changes, context)
            })?;
            return self.with_calculations(layout_id, record);
        }
        self.take_globals(layout_id, &mut values)?;
        let record = if self.in_transaction() {
            self.hold_create(layout_id, values)?
        } else {
            self.insert_native_record(layout_id, values)?
        };
        if let (Ok(layout), Ok(id)) = (self.decoded_layout(layout_id), u64::try_from(record.id)) {
            if let Ok(table) = self.table_for_layout(&layout).map(|t| t.id) {
                self.found_add(table, id);
            }
        }
        self.with_calculations(layout_id, record)
    }

    fn change_found_set(
        &mut self,
        layout_id: u32,
        change: om_data::found::FoundSetChange,
    ) -> Result<om_data::found::FoundSet, DataError> {
        self.change_found(layout_id, change)
    }

    fn update_record(
        &mut self,
        layout_id: u32,
        record_id: i64,
        mut values: om_data::FieldChanges,
    ) -> Result<Record, DataError> {
        self.require_writable()?;
        self.refuse_beside_record_session()?;
        self.refuse_while_unsettled()?;
        self.require_layout_records(layout_id)?;
        // Even a lost write reply may have changed the host. Draft and
        // transaction outcome handling remains with the existing write path.
        self.invalidate_calc_data();
        let layout = self.decoded_layout(layout_id)?;
        let table = self.read_table_for_layout(&layout)?;
        self.check_asset_write(
            &om_data::ScriptWrite {
                layout: layout_id,
                record: Some(record_id),
                fields: values.clone(),
                ..Default::default()
            },
            table.id,
        )?;
        if self.external.route(table.id).is_none() {
            self.require_action(table.id, om_data::host::security::RecordAction::Edit)?;
        }
        if self.external.route(table.id).is_some() {
            let record = self.with_existing_record_owner(table.id, record_id, |owner, wire| {
                let (table, plan) = owner.table_plan(wire)?;
                let changes = Self::writable_values(&table, &plan, &values)?;
                Self::check_scalar_cells(&table, &changes)?;
                let context = owner.write_context(&plan, None, records::CurrentSet::Account)?;
                owner.edit_table_record(&table, &plan, record_id, &changes, context)
            })?;
            return self.with_calculations(layout_id, record);
        }
        if self.take_globals(layout_id, &mut values)? && values.is_empty() {
            // Only global values were typed: the record itself is unchanged.
            return self.current_record(layout_id, record_id);
        }
        let record = if self.in_transaction() {
            self.hold_update(layout_id, record_id, values)?
        } else {
            self.edit_native_record(layout_id, record_id, values)?
        };
        self.with_calculations(layout_id, record)
    }

    fn supports_record_import(&self) -> bool {
        true
    }

    fn import_record(
        &mut self,
        layout_id: u32,
        record: Option<i64>,
        cells: &BTreeMap<u64, Option<String>>,
        auto_enter: bool,
    ) -> Result<i64, DataError> {
        self.refuse_while_unsettled()?;
        self.require_writable()?;
        self.require_layout_records(layout_id)?;
        // Even a lost write reply may have changed the host. Draft and
        // transaction outcome handling remains with the existing write path.
        self.invalidate_calc_data();
        let id = self.import_native_record(layout_id, record, cells, auto_enter)?;
        let layout = self.decoded_layout(layout_id)?;
        let table = self.table_for_layout(&layout)?.id;
        if record.is_none() {
            self.found_add(table, id);
        } else {
            self.calc.page.invalidate_record(table, id);
        }
        i64::try_from(id).map_err(|_| DataError::new("Record ID exceeds the supported range."))
    }

    fn prepare_record_edit(
        &mut self,
        request: &om_data::RecordEditRequest,
    ) -> Result<om_data::EditPreparation, DataError> {
        let result = self.prepare_root_record_edit(request);
        if matches!(result, Ok(om_data::EditPreparation::Unavailable)) {
            // Unknown versions/rules preserve ordinary entry, but no later
            // Insert may manufacture provenance after that entry was accepted.
            if let Ok((table, _)) = self.record_plan(request.baseline.layout) {
                let relevant = |field: &om_hbam::catalog::Field| {
                    field.kind == om_hbam::FieldKind::Normal
                        && field
                            .decoded_options()
                            .is_some_and(|o| o.options & 0x1_0000 == 0)
                };
                if self.external.route(table.id).is_none()
                    && (request
                        .fields
                        .iter()
                        .any(|n| table.field_by_name(n).is_some_and(relevant))
                        || request.cells.iter().any(|c| {
                            table
                                .field(*c)
                                .or_else(|| table.field(*c >> 8))
                                .is_some_and(relevant)
                        }))
                {
                    self.unprepared_root_edits.insert((
                        self.window,
                        table.id,
                        request.baseline.record,
                    ));
                }
            }
        }
        result
    }

    fn prepare_stored_container_edit(
        &mut self,
        baseline: om_data::ContainerEditBaseline,
        field: &str,
        value: &om_data::calc::Container,
    ) -> Result<om_data::EditPreparation, DataError> {
        self.prepare_stored_copy_entry(baseline, field, value)
    }

    fn prepare_container_edit(
        &mut self,
        layout_id: u32,
        record_id: i64,
        version: Option<u64>,
        field: &str,
    ) -> Result<bool, DataError> {
        self.prepare_existing_attachment(layout_id, record_id, version, field)
    }

    fn prepare_related_container_edit(
        &mut self,
        occurrence: u64,
        table: u64,
        record: i64,
        version: Option<u64>,
        field: u64,
        root: Option<om_data::ContainerEditBaseline>,
    ) -> Result<bool, DataError> {
        self.prepare_related_attachment(occurrence, table, record, version, field, root)
    }

    fn container_reference(
        &mut self,
        layout_id: u32,
        field: &str,
        value: &om_data::calc::Container,
    ) -> Result<Option<String>, DataError> {
        self.refuse_while_unsettled()?;
        self.require_writable()?;
        self.require_layout_records(layout_id)?;
        self.store_container(layout_id, field, value)
    }

    fn adjust_container(
        &mut self,
        layout_id: u32,
        reference: &str,
        delta: i32,
    ) -> Result<(), DataError> {
        // Taking a reference is a change; giving one back is cleanup.
        if delta > 0 {
            self.refuse_while_unsettled()?;
        }
        self.require_writable()?;
        self.adjust_container_reference(layout_id, reference, delta)
    }

    fn owned_container_drafts(&self) -> BTreeMap<(u64, String), u32> {
        self.owned_asset_counts()
    }

    fn related_container_reference(
        &mut self,
        table: u64,
        field: u64,
        value: &om_data::calc::Container,
    ) -> Result<Option<String>, DataError> {
        self.refuse_while_unsettled()?;
        self.require_writable()?;
        self.require_open()?;
        self.check_asset_acquisition(table, Some(field))?;
        if let Some(route) = self.external.route(table) {
            // Acquire on the actual library owner, under this ordinary save.
            self.join_owner_transaction(table, route)?;
            if self.prepared_attachment_group() && value.library.is_some() {
                return Err(DataError::with_code(
                    3,
                    "Stored-source acquisition is outside prepared attachment recovery.",
                ));
            }
            let result = self.external.with_owner(route, |owner, wire| {
                owner.require_writable()?;
                let (schema, _) = owner.table_plan(wire)?;
                let reference = owner.store_table_container(
                    &schema,
                    schema.field(field).map(|f| f.name.clone()),
                    value,
                    None,
                )?;
                let asset = reference
                    .as_deref()
                    .and_then(om_data::containers::reference)
                    .and_then(|m| owner.acquired_asset(wire, &m, 1));
                Ok((reference, asset))
            });
            self.collect_attachment_part(table);
            let (reference, asset) = result?;
            if !self.prepared_attachment_group() {
                if let Some(asset) = asset {
                    self.add_asset_receipt(table, asset);
                }
            }
            return Ok(reference);
        }
        let (table, _) = self.table_plan(table)?;
        self.store_table_container(
            &table,
            table.field(field).map(|f| f.name.clone()),
            value,
            None,
        )
    }

    fn adjust_related_container(
        &mut self,
        table: u64,
        reference: &str,
        delta: i32,
    ) -> Result<(), DataError> {
        // Taking a reference is a change; giving one back is cleanup.
        if delta > 0 {
            self.refuse_while_unsettled()?;
        }
        self.require_writable()?;
        self.require_open()?;
        if delta > 0 {
            self.check_asset_acquisition(table, None)?;
        }
        if let Some(route) = self.external.route(table) {
            let member = om_data::containers::reference(reference);
            if delta < 0 {
                if let Some(member) = &member {
                    self.begin_asset_release(table, member, delta.saturating_neg())?;
                }
            }
            let asset = self.external.with_owner(route, |owner, wire| {
                owner.adjust_table_container(wire, reference, delta)?;
                Ok(member
                    .as_deref()
                    .filter(|_| delta > 0)
                    .and_then(|member| owner.acquired_asset(wire, member, delta)))
            })?;
            if let Some(asset) = asset {
                self.add_asset_receipt(table, asset);
            }
            if delta < 0 {
                if let Some(member) = &member {
                    self.acknowledge_asset_release(table, member, delta.saturating_neg());
                }
            }
            return Ok(());
        }
        self.adjust_table_container(table, reference, delta)
    }

    fn delete_record(&mut self, layout_id: u32, record_id: i64) -> Result<(), DataError> {
        self.delete_layout_record(layout_id, record_id, false)
    }

    fn delete_record_in_script(&mut self, layout_id: u32, record_id: i64) -> Result<(), DataError> {
        self.delete_layout_record(layout_id, record_id, true)
    }

    fn require_records_through(&self, layout_id: u32) -> Result<(), DataError> {
        self.require_script_layout_records(layout_id)
    }

    fn delete_related_record(&mut self, table: u64, record_id: i64) -> Result<(), DataError> {
        self.refuse_while_unsettled()?;
        // Before any cache, lock or transaction change.
        self.require_writable()?;
        // Even a lost write reply may have changed the host. Draft and
        // transaction outcome handling remains with the existing write path.
        self.invalidate_calc_data();
        if self.external.route(table).is_some() {
            // A portal row of another file's table: that file deletes it
            // (related_owner.rs).
            return self.delete_routed_related(table, record_id);
        }
        self.refuse_imported_table(table)?;
        if self.in_transaction() {
            return self.hold_delete(table, record_id);
        }
        self.remove_native_record(table, record_id)?;
        if let Ok(record) = u64::try_from(record_id) {
            self.calc.page.invalidate_record(table, record);
        }
        Ok(())
    }

    fn preview_new_record(
        &mut self,
        write: &om_data::ScriptWrite,
    ) -> Result<Option<om_data::NewRecordPreview>, DataError> {
        self.preview_new(write, None)
    }

    fn preview_new_record_over(
        &mut self,
        write: &om_data::ScriptWrite,
        pending: &[om_data::PendingRecord],
    ) -> Result<Option<om_data::NewRecordPreview>, DataError> {
        self.preview_new(write, Some(pending))
    }

    fn compute_pending(
        &mut self,
        write: &om_data::ScriptWrite,
        others: &[om_data::PendingRecord],
    ) -> Result<Option<om_data::PendingValues>, DataError> {
        self.pending_values(write, others)
    }

    fn discard_prepared(&mut self, id: om_data::PreparedId) {
        self.discard_preview(id);
    }

    fn write_record(&mut self, write: &om_data::ScriptWrite) -> Result<Record, DataError> {
        self.refuse_while_unsettled()?;
        self.require_writable()?;
        self.require_script_layout_records(write.layout)?;
        if write.prepared.is_some() && write.record.is_some() {
            return Err(DataError::with_code(
                3,
                "A preview names a new record, not an existing one.",
            ));
        }
        // Even a lost write reply may have changed the host. Draft and
        // transaction outcome handling remains with the existing write path.
        self.invalidate_calc_data();
        let layout = self.decoded_layout(write.layout)?;
        let layout_table = self.read_table_for_layout(&layout)?;
        // A record made before the window read the table joins the saved
        // found set it starts on (found.rs).
        if write.record.is_none() && write.table.is_none_or(|t| t == layout_table.id) {
            self.restore_saved_found(&layout, &layout_table);
        }
        let layout_table = layout_table.id;
        let table = write.table.unwrap_or(layout_table);
        self.check_asset_write(write, table)?;
        if self.external.route(table).is_some() {
            // A related record of another file's table on a layout of this
            // file's table: that file writes it (related_owner.rs).
            if table != layout_table && self.external.route(layout_table).is_none() {
                return self.write_routed_related(write, table);
            }
            // Only the layout's own base table: an existing record, or a new
            // one (New Record, Duplicate Record) created by the owner.
            if table != layout_table {
                return Err(DataError::with_code(
                    3,
                    "Related-file script writes support only records of the layout's base table.",
                ));
            }
            // A layout of another file's table previews nothing (preview.rs).
            if write.prepared.is_some() {
                return Err(DataError::with_code(
                    3,
                    "A new record of another file's layout has no preview.",
                ));
            }
            // Inside the window's record session, or a script's transaction,
            // the owner holds it as one of the transaction's parts, as it
            // holds a portal row of its table (related_owner.rs): op71 under
            // the transaction, and nothing stored until it commits or
            // reverts (#892, #1006).
            if self.in_transaction() {
                // A record the session's new record is not on the host: the
                // window saw no version of it to check its save against.
                if let Some(id) = write.record.and_then(|id| u64::try_from(id).ok()) {
                    if self.routed_held_new(table, id) {
                        self.pinned.remove(&(self.window, table, id));
                    }
                }
                let record = self.write_routed_related(write, table)?;
                if write.record.is_none() {
                    let id = u64::try_from(record.id)
                        .map_err(|_| DataError::new("Invalid record ID."))?;
                    self.found_add(table, id);
                }
                return self.with_calculations(write.layout, record);
            }
            let script_write = |owner: &mut Self, wire| {
                let (table, plan) = owner.table_plan(wire)?;
                Self::check_scalar_cells(&table, &write.cells)?;
                let mut cells = write.cells.clone();
                for cell in &write.typed {
                    if let Some(Some(text)) = cells.get_mut(cell) {
                        *text = owner.check_typed_entry(wire, *cell, text)?;
                    }
                }
                // This file's script's elevation is not the owner's.
                let context = owner.write_context(&plan, None, records::CurrentSet::Account)?;
                owner.script_table_record(
                    &table,
                    &plan,
                    (write.record, None),
                    (&cells, &write.typed),
                    write.skip_validation,
                    context,
                )
            };
            let record = match write.record {
                Some(record_id) => self.with_existing_record_owner(table, record_id, script_write),
                None => self.with_new_record_owner(table, script_write),
            }?;
            return self.with_calculations(write.layout, record);
        }
        // What the user typed is stored as their own write stores it (a
        // typed `1/2/26` as `1/2/2026`); the rest as the script set it.
        let mut cells = write.cells.clone();
        for cell in &write.typed {
            if let Some(Some(text)) = cells.get_mut(cell) {
                *text = self.check_typed_entry(table, *cell, text)?;
            }
        }
        let record = self.script_native_record(
            table,
            write.record,
            (&cells, &write.typed),
            write.skip_validation,
            records::Origin {
                layout: Some(write.layout),
                set: records::CurrentSet::Step(write.privilege_set.clone()),
                record_number: write.record_number,
                prepared: write.prepared,
            },
        )?;
        if let Ok(id) = u64::try_from(record.id) {
            match write.record {
                None => self.found_add(table, id),
                Some(_) => self.calc.page.invalidate_record(table, id),
            }
        }
        if table == layout_table {
            self.with_calculations(write.layout, record)
        } else {
            Ok(record)
        }
    }

    fn check_entry(&mut self, table: u64, cell: u64, text: &str) -> Result<(), DataError> {
        self.calc_schema_cached()?;
        if let Some(route) = self.external.route(table) {
            // A transaction holds a related file's record in that file's
            // part of it (`write_record`, #892, #1006).
            return self.with_owner_reading_root(route, |owner, wire| {
                let (table, _) = owner.table_plan(wire)?;
                Self::check_scalar_cells(&table, &[(cell, Some(text.to_owned()))].into())?;
                owner.check_script_entry(wire, cell, text)
            });
        }
        self.check_script_entry(table, cell, text)
    }

    fn set_typed_global(
        &mut self,
        table: u64,
        field: u64,
        repetition: u32,
        text: &str,
    ) -> Result<(), DataError> {
        self.refuse_while_unsettled()?;
        self.require_open()?;
        self.refuse_imported_table(table)?;
        self.calc_schema_cached()?;
        let def = self
            .catalog
            .table(table)
            .and_then(|t| t.field(field))
            .ok_or_else(|| DataError::with_code(102, "The field is missing."))?;
        let supported = def.kind == om_hbam::FieldKind::Normal
            && matches!(
                def.data_type,
                om_hbam::DataType::Date | om_hbam::DataType::Time | om_hbam::DataType::Timestamp
            )
            && repetition >= 1
            && def.decoded_options().is_some_and(|options| {
                options.options & 0x1_0000 != 0
                    && repetition <= u32::from(options.max_repetitions.max(1))
                    && options.validation_options == 0
                    && !options.auto_entry_editing_prohibited()
            });
        if !supported {
            return Err(DataError::with_code(
                3,
                "Typed entry requires a supported repetition of a plain temporal global.",
            ));
        }
        // Reuse ordinary entry's locale-aware parsing, without changing a
        // script's permissive global assignment. Refusal changes no value.
        let cell = om_data::rules::cell_of(field, repetition)
            .ok_or_else(|| DataError::with_code(201, "The repetition is out of range."))?;
        let text = self.check_typed_entry(table, cell, text)?;
        self.set_global(table, field, repetition, &text)
    }

    /// The session's own value, never uploaded: calculations read it over
    /// the stored one until the file is closed.
    fn set_global(
        &mut self,
        table: u64,
        field: u64,
        repetition: u32,
        text: &str,
    ) -> Result<(), DataError> {
        self.refuse_while_unsettled()?;
        self.require_open()?;
        self.refuse_imported_table(table)?;
        let schema = self.calc_schema_cached()?;
        let def = schema
            .table(table)
            .and_then(|t| t.fields.get(&field))
            .ok_or_else(|| DataError::with_code(102, "The field is missing."))?;
        if !def.global || repetition == 0 || repetition > def.repetitions.max(1) {
            return Err(DataError::with_code(
                201,
                format!("Field {} is not a global field's repetition.", def.name),
            ));
        }
        // A global the account's custom set gives view-only access keeps its
        // value: Set Field and Browse's typing both come here, not through
        // `prepare_record`.
        let modifies = self.account_info.set_access.as_ref().is_none_or(|access| {
            access.modifies(
                &om_hbam::slot::uint_segment(table),
                &om_hbam::slot::uint_segment(field),
            )
        });
        if !modifies {
            return Err(DataError::with_code(201, FIELD_NOT_MODIFIABLE));
        }
        self.invalidate_calc_data();
        self.set_desktop_global(&schema, table, field, repetition, text);
        self.calc
            .globals
            .insert((table, field, repetition), text.to_string());
        Ok(())
    }

    fn use_window(&mut self, window: u32) {
        if window == self.window {
            return;
        }
        // The window's record session stays with it: the next window's
        // writes are not held in it. A script's transaction stays open.
        if self.transaction_kind() == Some(transaction::TransactionKind::Record) {
            let session = self.set_aside();
            self.record_sessions.insert(self.window, session);
        }
        let found = self.windows.remove(&window).unwrap_or_default();
        let previous = std::mem::replace(&mut self.found, found);
        self.windows.insert(self.window, previous);
        self.window = window;
        self.resume_window_session();
        // The last page's found set was the other window's.
        self.calc.page = om_data::host::CalcData::default();
    }

    fn copy_window(&mut self, from: u32, to: u32) {
        // Copying a found set does not copy another window's checked drafts.
        let found = if from == self.window {
            self.found.clone()
        } else {
            self.windows.get(&from).cloned().unwrap_or_default()
        };
        if to == self.window {
            self.calc.page = om_data::host::CalcData::default();
            self.found = found;
        } else {
            self.windows.insert(to, found);
        }
        self.copy_saved_found(from, to);
    }

    fn drop_window(&mut self, window: u32) {
        self.unprepared_root_edits.retain(|(w, _, _)| *w != window);
        // Settle or detach this window's acquisition receipt before ending its
        // transactions. An unresolved receipt keeps the only cleanup authority.
        if let Err(error) = self.discard_window_assets(window) {
            log::warn!("closing a window retained unresolved container ownership: {error}");
        }
        self.discard_unsettled(window);
        // What a failed reopen carried for it goes with it (#152).
        self.carried.retain(|carried| carried.window() != window);
        self.unrecreated.remove(&window);
        // Its record session ends as Revert ends it (op65), its parts
        // settled first: the open one, or one set aside, the current
        // window's too while another window's script transaction is open,
        // which stays open.
        if window == self.window
            && self.transaction_kind() == Some(transaction::TransactionKind::Record)
        {
            let _ = self.revert_with_owner();
        }
        if let Some(session) = self.record_sessions.remove(&window) {
            self.end_parked_session(session);
        }
        self.pinned.retain(|(owner, _, _), _| *owner != window);
        self.drop_saved_found(window);
        if window == self.window {
            self.calc.page = om_data::host::CalcData::default();
            self.found.clear();
        } else {
            self.windows.remove(&window);
        }
    }

    fn value_list(&mut self, id: u64) -> Result<Vec<String>, DataError> {
        self.read_value_list(id)
    }

    fn host_field_values(
        &mut self,
        table: u64,
        field: u64,
    ) -> Result<Option<Vec<String>>, DataError> {
        NativeProvider::host_field_values(self, table, field)
    }

    fn index_values(&mut self, layout_id: u32, field: &str) -> Result<Vec<String>, DataError> {
        self.read_index_values(layout_id, field)
    }

    fn set_next_serial_value(
        &mut self,
        occurrence: u64,
        field: u64,
        value: &str,
    ) -> Result<(), DataError> {
        self.refuse_while_unsettled()?;
        self.require_writable()?;
        self.write_next_serial(occurrence, field, value)
    }

    fn begin_transaction(
        &mut self,
        options: om_data::runtime::TransactionOptions,
    ) -> Result<(), DataError> {
        self.refuse_while_unsettled()?;
        self.begin_kind(options, transaction::TransactionKind::Script)?;
        self.owner_transaction = related_owner::OwnerTransaction::opened(options);
        Ok(())
    }

    fn begin_record_commit(&mut self) -> Result<(), DataError> {
        self.refuse_while_unsettled()?;
        let options = om_data::runtime::TransactionOptions::default();
        self.begin_kind(options, transaction::TransactionKind::Record)?;
        self.owner_transaction = related_owner::OwnerTransaction::opened(options);
        Ok(())
    }

    fn commit_record_session(&mut self, skip_validation: bool) -> Result<(), DataError> {
        self.refuse_while_unsettled()?;
        self.require_writable()?;
        self.validate_asset_change()?;
        // Kept on a refusal only while the commit is this file's alone; one
        // that opened a related file's transaction commits as Commit
        // Transaction does (related_owner.rs).
        let mark = self.submission_mark();
        let committed = if self.transaction_kind() == Some(transaction::TransactionKind::Record)
            && self.owner_transaction.alone()
        {
            let committed = self.commit_keeping(skip_validation);
            if committed.is_ok() || self.transaction.is_none() {
                self.owner_transaction = Default::default();
            }
            committed
        } else {
            self.commit_with_owner(skip_validation, true)
        };
        self.scalar_refused(mark, &committed);
        // A submitted group belongs to the existing participant outcome ledger;
        // a fresh no-data refusal captured a new receipt above.
        if self
            .asset_settlement
            .as_ref()
            .is_some_and(|c| c.window() == self.window)
        {
            self.asset_settlement = None;
        }
        self.invalidate_calc_data();
        committed
    }

    fn unsettled_save(&mut self) -> Option<Vec<om_data::FileCommit>> {
        self.unsettled_report()
    }

    fn unsettled_saves(&mut self) -> Vec<(u32, Vec<om_data::FileCommit>)> {
        self.unsettled_reports()
    }

    fn reconcile_save(&mut self) -> Result<Vec<om_data::FileCommit>, DataError> {
        self.reconcile_unsettled()
    }

    fn resume_save(&mut self) -> Result<(), DataError> {
        self.resume_unsettled()
    }

    fn discard_save(&mut self) -> Result<(), DataError> {
        if self.discard_unsettled(self.window) {
            Ok(())
        } else {
            Err(DataError::with_code(
                3,
                "This window has no partly saved record to settle.",
            ))
        }
    }

    fn record_session_open(&self) -> bool {
        self.transaction_kind() == Some(transaction::TransactionKind::Record)
    }

    fn transaction_open(&self) -> bool {
        self.in_transaction()
    }

    fn record_session_held(&self) -> bool {
        self.record_session_open() || self.record_sessions.contains_key(&self.window)
    }

    fn revert_record_session(&mut self) -> Result<(), DataError> {
        self.discard_scalar_assets()?;
        self.unprepared_root_edits
            .retain(|(w, _, _)| *w != self.window);
        // A failed reconnect may have closed the original transaction while
        // retaining its immutable issued values. Revert still discards them.
        if self
            .scalar_carry
            .as_ref()
            .is_some_and(|carry| carry.window() == self.window)
        {
            self.scalar_carry = None;
            self.scalar_refusal = None;
        }
        // Revert Record of a save that may be partly applied discards what
        // was not saved; the files that saved keep it (#1006).
        if self.discard_unsettled(self.window) {
            return Ok(());
        }
        if self.record_session_open() {
            return self.revert_transaction();
        }
        if let Some(session) = self.record_sessions.remove(&self.window) {
            self.end_parked_session(session);
            self.invalidate_calc_data();
        }
        Ok(())
    }

    fn enter_record_session(&mut self) -> bool {
        self.enter_session()
    }

    fn leave_record_session(&mut self) {
        self.leave_session();
    }

    fn mark_record_session(&mut self) {
        self.mark_session();
    }

    fn rollback_record_session(&mut self) {
        self.rollback_session();
    }

    fn commit_transaction(&mut self, skip_validation: bool) -> Result<(), DataError> {
        self.refuse_while_unsettled()?;
        self.require_writable()?;
        self.validate_asset_change()?;
        let committed = self.commit_with_owner(skip_validation, false);
        // Written or rolled back (a refused or failed commit reverts what it
        // held), what calculations read while records were held is not the
        // host's now (#1042).
        self.invalidate_calc_data();
        self.resume_window_session();
        committed
    }

    fn revert_transaction(&mut self) -> Result<(), DataError> {
        self.discard_scalar_assets()?;
        if self.discard_unsettled(self.window) {
            return Ok(());
        }
        // What calculations read while records were held (a page's record
        // list without a held deletion, say) is not the host's once they
        // are released: read again (#1042).
        let reverted = self.revert_with_owner();
        self.invalidate_calc_data();
        self.resume_window_session();
        reverted
    }

    fn account_info(&self) -> om_data::host::AccountInfo {
        self.account_info.clone()
    }

    fn account_privileges(&self) -> Option<om_data::host::Privileges> {
        self.privileges.account.clone()
    }

    fn full_access_privileges(&self) -> Option<om_data::host::Privileges> {
        self.privileges.full_access.clone()
    }

    fn host_application_version(&self) -> Option<String> {
        Some(self.host_version.clone()).filter(|v| !v.is_empty())
    }

    fn calc_schema(
        &mut self,
    ) -> Result<Option<std::sync::Arc<om_data::host::CalcSchema>>, DataError> {
        self.calc_schema_cached().map(Some)
    }

    fn refresh_window(
        &mut self,
        flush_joins: bool,
        _flush_external: bool,
    ) -> Result<(), DataError> {
        self.require_open()?;
        self.invalidate_calc_notifications();
        self.calc.page.refresh_window(flush_joins);
        // Relationship scans are join results too (#804).
        if flush_joins {
            self.calc.scans.get_mut().clear();
        }
        self.calc.generation = self.calc.generation.wrapping_add(1);
        // This backend reads hosted FileMaker files, including imported
        // owning routes, through calc.page. It has no ODBC query cache:
        // external-data flush is a successful no-op, not a peer-file flush.
        // Keep sessions, schema, library budgets, globals and held writes.
        Ok(())
    }

    fn refresh_object(&mut self, join: Option<(u64, u64, u64)>) -> Result<Option<u64>, DataError> {
        self.require_open()?;
        self.invalidate_calc_notifications();
        self.calc.page.refresh_object(join);
        // A join it flushes may have been matched by a scan (#804).
        if join.is_some() {
            self.calc.scans.get_mut().clear();
        }
        self.calc.generation = self.calc.generation.wrapping_add(1);
        Ok(Some(self.calc.generation))
    }

    fn calc_generation(&self) -> Option<u64> {
        Some(self.calc.generation)
    }

    fn calc_data(
        &mut self,
        needs: &[om_data::host::CalcNeed],
    ) -> Result<om_data::host::CalcData, DataError> {
        self.read_calc_data(needs)
    }

    fn calc_data_for(
        &mut self,
        needs: &[om_data::host::CalcNeed],
        cancel: &om_data::host::CalcCancel,
    ) -> Result<om_data::host::CalcData, DataError> {
        self.read_calc_data_for(needs, Some(cancel))
    }

    fn scripts(&mut self) -> Result<om_data::ScriptCatalog, DataError> {
        self.read_scripts()
    }

    fn script(&mut self, id: u64) -> Result<Option<om_data::Script>, DataError> {
        self.read_script(id)
    }

    fn scripts_for_editing(&mut self) -> Result<om_data::ScriptCatalog, DataError> {
        self.read_scripts_for_editing()
    }

    fn save_script(&mut self, script: om_data::Script) -> Result<om_data::Script, DataError> {
        self.save_edited_script(script)
    }

    fn save_script_catalog(
        &mut self,
        catalog: om_data::ScriptCatalog,
    ) -> Result<om_data::ScriptCatalog, DataError> {
        self.save_catalog(catalog)
    }

    fn perform_script_on_server(
        &mut self,
        call: &om_data::runtime::ServerScriptCall,
    ) -> Result<om_data::runtime::ServerScriptResult, DataError> {
        self.server_script(call)
    }

    fn perform_script_on_server_with_callback(
        &mut self,
        call: &om_data::runtime::ServerScriptCall,
        callback: u32,
    ) -> Result<om_data::runtime::ErrorCode, DataError> {
        self.server_script_with_callback(call, callback)
    }

    fn server_script_results(
        &mut self,
        wait: std::time::Duration,
    ) -> Result<Vec<om_data::ServerScriptCallback>, DataError> {
        self.callback_results(wait)
    }

    fn database_schema(&mut self) -> Result<om_data::DatabaseSchema, DataError> {
        self.database_schema_for_editing()
    }

    fn security_catalog(
        &mut self,
    ) -> Result<om_data::security_catalog::SecurityCatalog, DataError> {
        // FileMaker's error 9 (insufficient privileges) for a refused
        // account, 3 (unsupported) for a host without the operation.
        self.security().map_err(|error| match error {
            AuthoringError::Refused {
                status, message, ..
            } => DataError::with_code(i32::from(status), message),
            AuthoringError::Unavailable(reason) => DataError::with_code(3, reason),
            other => DataError::new(other.to_string()),
        })
    }

    fn begin_schema_edit(&mut self) -> Result<om_data::DatabaseSchema, DataError> {
        self.begin_schema_edit_on_host()
    }

    fn apply_schema_change(
        &mut self,
        change: om_data::SchemaChange,
    ) -> Result<om_data::DatabaseSchema, DataError> {
        self.require_writable()?;
        self.stage_schema_change(change)
    }

    fn commit_schema_changes(&mut self) -> Result<om_data::DatabaseSchema, DataError> {
        self.require_writable()?;
        self.commit_schema_edit()
    }

    fn cancel_schema_changes(&mut self) -> Result<(), DataError> {
        self.cancel_schema_edit()
    }

    fn place_layout_field(
        &mut self,
        place: om_data::layout_placement::LayoutFieldPlacement,
    ) -> Result<om_data::layout_placement::PlacedLayoutField, DataError> {
        self.require_writable()?;
        let placed = self.place_field_on_layout(&place);
        // Whatever the host answered, this window's layouts are read again,
        // so they show what the host stores (with an unknown outcome, too).
        let refreshed = self.refresh_layout_downloads();
        let placed = placed?;
        refreshed?;
        Ok(placed)
    }

    fn set_layout_object_frames(
        &mut self,
        change: om_data::layout_objects::LayoutObjectFrames,
    ) -> Result<om_data::layout_objects::FramedObjects, DataError> {
        self.require_writable()?;
        let framed = self.set_frames_on_layout(&change);
        // Whatever the host answered, this window's layouts are read again,
        // so they show what the host stores (with an unknown outcome, too).
        let refreshed = self.refresh_layout_downloads();
        let framed = framed?;
        refreshed?;
        Ok(framed)
    }

    fn set_field_control(
        &mut self,
        change: om_data::layout_objects::FieldControlChange,
    ) -> Result<om_data::layout_objects::FramedObjects, DataError> {
        self.require_writable()?;
        let set = self
            .field_control_on_host(om_protocol::authoring::FieldControl {
                expected: change.expected,
                layout: u64::from(change.layout),
                object: change.object,
                control: change.control,
                value_list: change.value_list,
            })
            .map(|framed| om_data::layout_objects::FramedObjects {
                changed: framed.changed,
                revision: framed.revision,
            })
            .map_err(|error| object_edit_error(error, "The field control was"));
        // As for moves: the layouts are read again whatever the answer.
        let refreshed = self.refresh_layout_downloads();
        let set = set?;
        refreshed?;
        Ok(set)
    }

    fn set_layout_text(
        &mut self,
        change: om_data::layout_placement::TextChange,
    ) -> Result<om_data::layout_objects::FramedObjects, DataError> {
        self.require_writable()?;
        let set = self
            .text_on_host(om_protocol::authoring::SetText {
                expected: change.expected,
                layout: u64::from(change.layout),
                object: change.object,
                text: change.text,
            })
            .map(|framed| om_data::layout_objects::FramedObjects {
                changed: framed.changed,
                revision: framed.revision,
            })
            .map_err(|error| object_edit_error(error, "The text was"));
        // As for moves: the layouts are read again whatever the answer.
        let refreshed = self.refresh_layout_downloads();
        let set = set?;
        refreshed?;
        Ok(set)
    }

    fn set_layout_parts(
        &mut self,
        change: om_data::layout_placement::PartsChange,
    ) -> Result<om_data::layout_objects::FramedObjects, DataError> {
        use om_data::layout_placement::PartChange;
        self.require_writable()?;
        let (action, part, part_type, size) = match change.change {
            PartChange::Add { part_type, size } => {
                (om_protocol::authoring::PARTS_ADD, 0, part_type, size)
            }
            PartChange::Delete { part, part_type } => {
                (om_protocol::authoring::PARTS_DELETE, part, part_type, 0.0)
            }
            PartChange::Resize {
                part,
                part_type,
                size,
            } => (om_protocol::authoring::PARTS_RESIZE, part, part_type, size),
        };
        let part_type = u32::try_from(part_type)
            .map_err(|_| DataError::with_code(5, "No part has that type."))?;
        let set = self
            .parts_on_host(om_protocol::authoring::ChangeParts {
                expected: change.expected,
                layout: u64::from(change.layout),
                action,
                part,
                part_type,
                size,
            })
            .map(|framed| om_data::layout_objects::FramedObjects {
                changed: framed.changed,
                revision: framed.revision,
            })
            .map_err(|error| object_edit_error(error, "The change of parts was"));
        // As for moves: the layouts are read again whatever the answer.
        let refreshed = self.refresh_layout_downloads();
        let set = set?;
        refreshed?;
        Ok(set)
    }

    fn set_part_options(
        &mut self,
        change: om_data::layout_placement::PartOptionsChange,
    ) -> Result<om_data::layout_objects::FramedObjects, DataError> {
        self.require_writable()?;
        let set = self
            .part_options_on_host(om_protocol::authoring::PartOptions {
                expected: change.expected,
                layout: u64::from(change.layout),
                part: change.part,
                flags: change.flags,
                page_break_after: change.page_break_after,
                break_field: change.break_field,
            })
            .map(|framed| om_data::layout_objects::FramedObjects {
                changed: framed.changed,
                revision: framed.revision,
            })
            .map_err(|error| object_edit_error(error, "The part's options were"));
        // As for moves: the layouts are read again whatever the answer.
        let refreshed = self.refresh_layout_downloads();
        let set = set?;
        refreshed?;
        Ok(set)
    }

    fn set_layout_width(
        &mut self,
        change: om_data::layout_placement::LayoutWidthChange,
    ) -> Result<om_data::layout_objects::FramedObjects, DataError> {
        self.require_writable()?;
        let set = self
            .layout_width_on_host(om_protocol::authoring::LayoutWidth {
                expected: change.expected,
                layout: u64::from(change.layout),
                width: change.width,
            })
            .map(|framed| om_data::layout_objects::FramedObjects {
                changed: framed.changed,
                revision: framed.revision,
            })
            .map_err(|error| object_edit_error(error, "The width was"));
        // As for moves: the layouts are read again whatever the answer.
        let refreshed = self.refresh_layout_downloads();
        let set = set?;
        refreshed?;
        Ok(set)
    }

    fn set_layout_setup(
        &mut self,
        change: om_data::layout_catalog::LayoutSetupChange,
    ) -> Result<om_data::layout_objects::FramedObjects, DataError> {
        self.require_writable()?;
        let set = self
            .layout_setup_on_host(om_protocol::authoring::LayoutSetup {
                expected: change.expected,
                layout: u64::from(change.layout),
                name: change.name,
                flags: change.flags,
                hidden: change.hidden,
            })
            .map(|framed| om_data::layout_objects::FramedObjects {
                changed: framed.changed,
                revision: framed.revision,
            })
            .map_err(|error| object_edit_error(error, "Layout Setup was"));
        // The name is in the layout list: it is read again whatever the
        // answer.
        let refreshed = self.refresh_layouts();
        let set = set?;
        refreshed?;
        Ok(set)
    }

    fn delete_layout_objects(
        &mut self,
        change: om_data::layout_objects::LayoutObjectDeletion,
    ) -> Result<om_data::layout_objects::DeletedObjects, DataError> {
        self.require_writable()?;
        let deleted = self.delete_on_layout(&change);
        // As for moves: the layouts are read again whatever the answer.
        let refreshed = self.refresh_layout_downloads();
        let deleted = deleted?;
        refreshed?;
        Ok(deleted)
    }

    fn create_layout(
        &mut self,
        request: om_data::layout_catalog::LayoutRequest,
    ) -> Result<om_data::layout_catalog::CreatedLayout, DataError> {
        self.require_writable()?;
        let created = self.create_layout_with(&request);
        // Whatever the host answered, the layouts are read again, so the
        // window lists what the host stores (with an unknown outcome, too).
        let refreshed = self.refresh_layouts();
        let created = created?;
        refreshed?;
        Ok(created)
    }

    fn delete_layout(
        &mut self,
        layout: u32,
    ) -> Result<om_data::layout_catalog::RemovedLayout, DataError> {
        self.require_writable()?;
        let deleted = self.delete_layout_with(layout);
        // As for a new layout: the layouts are read again whatever the
        // answer.
        let refreshed = self.refresh_layouts();
        let deleted = deleted?;
        refreshed?;
        Ok(deleted)
    }

    fn duplicate_layout(
        &mut self,
        layout: u32,
    ) -> Result<om_data::layout_catalog::CreatedLayout, DataError> {
        self.require_writable()?;
        let created = self.duplicate_layout_with(layout);
        // As for a new layout: the layouts are read again whatever the
        // answer.
        let refreshed = self.refresh_layouts();
        let created = created?;
        refreshed?;
        Ok(created)
    }

    fn place_layout_button(
        &mut self,
        place: om_data::layout_placement::LayoutButtonPlacement,
    ) -> Result<om_data::layout_placement::PlacedLayoutButton, DataError> {
        self.require_writable()?;
        let placed = self.place_button_on_layout(&place);
        let refreshed = self.refresh_layout_downloads();
        let placed = placed?;
        refreshed?;
        Ok(placed)
    }

    fn place_layout_portal(
        &mut self,
        place: om_data::layout_placement::LayoutPortalPlacement,
    ) -> Result<om_data::layout_placement::PlacedLayoutPortal, DataError> {
        self.require_writable()?;
        let placed = self.place_portal_on_layout(&place);
        // As for the other placements: read again whatever the answer.
        let refreshed = self.refresh_layout_downloads();
        let placed = placed?;
        refreshed?;
        Ok(placed)
    }

    fn place_layout_text(
        &mut self,
        place: om_data::layout_placement::LayoutTextPlacement,
    ) -> Result<om_data::layout_placement::PlacedLayoutText, DataError> {
        self.require_writable()?;
        let placed = self.place_text_on_layout(&place);
        // As for the other placements: read again whatever the answer.
        let refreshed = self.refresh_layout_downloads();
        let placed = placed?;
        refreshed?;
        Ok(placed)
    }

    fn list_files(&mut self, host: &str, port: u16) -> Result<Vec<HostedFile>, DataError> {
        use om_data::HostBrowser;
        browse::NativeBrowser::new(self.config.clone())
            .browse(host, port, &std::sync::atomic::AtomicBool::new(false))
            .map(|listing| listing.files)
            .map_err(DataError::from)
    }

    fn host_browser(&self) -> Option<std::sync::Arc<dyn om_data::HostBrowser>> {
        Some(std::sync::Arc::new(browse::NativeBrowser::new(
            self.config.clone(),
        )))
    }

    fn database_sources(&mut self) -> Result<Vec<om_data::file_refs::FileDataSource>, DataError> {
        self.require_open()?;
        // A file that never had a data source has no catalog for them.
        let Some(bytes) = download_node(self.session.as_mut().expect("open"), vec![key_bytes(32)])?
        else {
            return Ok(Vec::new());
        };
        let downloads = [bytes];
        let tree = download_tree(&downloads)?;
        let Some(sources) = tree.at(&[32, 5]) else {
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        for source in &sources.children {
            let Some(raw) = source.id() else {
                continue;
            };
            let Some(id) = om_hbam::serial::decode(source.segment.0) else {
                continue;
            };
            let name = tree
                .at(&[32, 5, raw, 16])
                .and_then(om_hbam::Node::text)
                .and_then(Result::ok)
                .unwrap_or_default();
            let paths = tree
                .at(&[32, 5, raw, 130])
                .and_then(|n| n.assembled())
                .and_then(|b| om_data::file_refs::decode(&b))
                .unwrap_or_default();
            result.push(om_data::file_refs::FileDataSource { id, name, paths });
        }
        Ok(result)
    }

    fn value_list_catalog(&mut self) -> Result<om_data::value_lists::ValueListCatalog, DataError> {
        self.read_value_list_catalog()
    }

    fn custom_value_list(
        &mut self,
        list: u64,
    ) -> Result<om_data::value_lists::CustomList, DataError> {
        use om_data::value_lists::{self, CustomList, ValuesRefusal};
        use om_protocol::authoring::{catalog_revision, status};
        let member = self.value_list_member(list)?.ok_or_else(|| {
            DataError::with_code(
                i32::from(status::VALUE_LIST_MISSING),
                format!("The file has no value list {list}."),
            )
        })?;
        let (name, values) = value_lists::member_values(&member).map_err(|refusal| {
            let code = match refusal {
                ValuesRefusal::Missing(_) => status::VALUE_LIST_MISSING,
                ValuesRefusal::Unsupported(_) => status::UNSUPPORTED,
                ValuesRefusal::Invalid(_) => status::INVALID,
            };
            DataError::with_code(i32::from(code), refusal.to_string())
        })?;
        Ok(CustomList {
            name,
            values,
            revision: catalog_revision(&member),
        })
    }

    fn set_value_list_values(
        &mut self,
        change: om_data::value_lists::ValuesChange,
    ) -> Result<om_data::layout_objects::FramedObjects, DataError> {
        self.require_writable()?;
        let set = self
            .value_list_values_on_host(om_protocol::authoring::ValueListValues {
                expected: change.expected,
                list: change.list,
                values: change.values,
            })
            .map(|framed| om_data::layout_objects::FramedObjects {
                changed: framed.changed,
                revision: framed.revision,
            })
            .map_err(|error| match error {
                authoring::AuthoringError::OutcomeUnknown(reason) => DataError::new(format!(
                    "The value list's change was sent but no answer came back ({reason}), so \
                     whether the host saved it is not known. The value lists are read again."
                )),
                other => object_edit_error(other, "The value list's change was"),
            });
        // The menus read value lists from the calculation schema: it is
        // read again whatever the answer.
        self.drop_calc_schema();
        set
    }

    fn references_peer(&mut self, file: &str) -> Result<bool, DataError> {
        if self.local.is_some() {
            // The related files it opened beside it: names are unique in
            // the family (local.rs), so the name is the file.
            return Ok(self.external.has_local_file(file));
        }
        let sources = self.database_sources()?;
        let used: std::collections::BTreeSet<_> = self
            .catalog
            .table_occurrences
            .iter()
            .filter_map(|o| {
                o.reference()
                    .and_then(|(source, _)| om_hbam::serial::id_of_raw(source))
            })
            .collect();
        let Some(auth) = self.peer_auth.as_ref() else {
            return Ok(false);
        };
        // The file on its host and port: this file's for a plain name, or
        // the one `fmnet:/host:port/Name` gives (#976); each data source
        // path on the host and port it names, a relative one on this
        // file's. One name on two hosts is two files.
        let (host, port, file, _) = peer_target(auth, file);
        let key = om_data::file_refs::hosted_key(&file);
        Ok(sources
            .iter()
            .filter(|s| used.contains(&s.id))
            .flat_map(|s| &s.paths)
            .filter_map(|p| match p.same_host_name(&auth.host, auth.port) {
                Some(name) => Some((auth.host.clone(), auth.port, name)),
                None => {
                    let (there, there_port) = p.fmnet_endpoint()?;
                    om_data::file_refs::is_plain_name(&p.name)
                        .then(|| (there.to_owned(), there_port, p.name.clone()))
                }
            })
            .any(|(there, there_port, name)| {
                there_port == port
                    && there.eq_ignore_ascii_case(&host)
                    && om_data::file_refs::hosted_key(&name) == key
            }))
    }

    fn session_account(&self) -> Option<String> {
        Some(self.account_name.clone())
    }

    fn server_port(&self) -> u16 {
        self.peer_auth.as_ref().map_or(5003, |a| a.port)
    }

    fn local_path(&self) -> Option<std::path::PathBuf> {
        self.session.as_ref()?;
        self.local.as_ref().map(|host| host.path().to_path_buf())
    }

    fn local_peer_path(&self, file: &str) -> Option<std::path::PathBuf> {
        self.session.as_ref()?;
        let folder = self.local.as_ref()?.path().parent()?;
        om_data::file_refs::is_plain_name(file)
            .then(|| folder.join(om_data::file_refs::hosted_name(file)))
    }

    fn open_peer(&mut self, file: &str) -> Result<Box<dyn DataProvider>, DataError> {
        self.open_peer_with(file, None)
    }

    fn open_peer_as(
        &mut self,
        file: &str,
        account: &str,
        password: &str,
    ) -> Result<Box<dyn DataProvider>, DataError> {
        self.open_peer_with(file, Some((account, password)))
    }

    fn peer_refused(&self, file: &str, credentials: Option<(&str, &str)>) -> bool {
        let (key, session) = if self.local.is_some() {
            let Some(path) = self.local_peer_path(file) else {
                return false;
            };
            let session = self
                .local_auth
                .as_ref()
                .and_then(|a| Some((a.account.as_str(), a.password.as_deref()?.as_str())));
            (refusals::FileKey::local(&path), session)
        } else {
            let Some(auth) = &self.peer_auth else {
                return false;
            };
            let (host, port, file, elsewhere) = peer_target(auth, file);
            // Another host's file waits for an account of its own once none
            // is sent there ([`Self::other_host_account`]): refused there, or
            // not held. One that failed otherwise (no such file there, no
            // verified connection) does not ask.
            if elsewhere && credentials.is_none() {
                return self.other_host_account(&host, port, &file).is_none();
            }
            let session = auth
                .password
                .as_deref()
                .filter(|_| !elsewhere)
                .map(|password| (auth.account.as_str(), password.as_str()));
            (refusals::FileKey::hosted(&host, port, &file), session)
        };
        let Some((account, password)) = credentials.or(session) else {
            return false;
        };
        self.login_refusals
            .lock()
            .expect("login refusals")
            .refused(&key, account, password)
            .is_some()
    }

    fn open(&mut self, connection: &Connection) -> Result<(), DataError> {
        // A new session: no file has refused its credentials yet.
        self.login_refusals = Default::default();
        self.open_catalog(connection, true)
    }

    /// A fresh Connect, Open and Login (`Reconnect` has never been observed,
    /// network-protocol.md 6.3). The windows' found sets are lists of the
    /// file's record ids the client holds: they carry over, and `found_ids`
    /// leaves out records deleted since. Versions read, held transactions
    /// and locks belonged to the lost session and do not.
    ///
    /// The fresh login's credentials have the ordinary session lifetime:
    /// kept in zeroized memory for later related reads and script opens,
    /// and discarded when the provider closes or replaces the session.
    fn reopen(&mut self, connection: &Connection) -> Result<(), DataError> {
        if self.local.is_some() {
            return Err(DataError::with_code(
                802,
                "Reconnect this local file by its path.",
            ));
        }
        // The same session: the files that refused it are not tried again.
        // The other files that were on the lost connection may reopen on
        // the new one, for the same host, port and account
        // ([`DataProvider::reopen_on_successor`]).
        let lost = self
            .session
            .as_ref()
            .filter(|session| session.connection_lost().is_some())
            .map(Session::share);
        self.reopen_with(|provider| provider.open_catalog(connection, true))?;
        if let (Some(lost), Some(session)) = (&lost, &self.session) {
            lost.hand_over(
                session,
                &connection.host,
                connection.port,
                refusals::admission(&connection.account, &connection.password),
            );
        }
        Ok(())
    }

    fn reopen_on_successor(&mut self) -> Result<bool, DataError> {
        if self.local.is_some() {
            return Ok(false);
        }
        let Some(session) = self
            .session
            .as_ref()
            .filter(|session| session.connection_lost().is_some())
        else {
            return Ok(false);
        };
        let Some(PeerAuth {
            host,
            port,
            account,
            password: Some(password),
        }) = &self.peer_auth
        else {
            return Ok(false);
        };
        let Some(file) = session.opened_file().map(|opened| opened.filename.clone()) else {
            return Ok(false);
        };
        let admission = refusals::admission(account, password);
        let Some(shared) = session.successor(host, *port, &admission) else {
            return Ok(false);
        };
        // A file that refused these credentials in this session is not sent
        // them again (refusals.rs), and a refusal now is remembered: nothing
        // is tried again, the file stays lost.
        let key = refusals::FileKey::hosted(host, *port, &file);
        let refusals = self.login_refusals.clone();
        if let Some(error) = refusals
            .lock()
            .expect("login refusals")
            .refused(&key, account, password)
        {
            return Err(error);
        }
        let mut connection = Connection {
            host: host.clone(),
            port: *port,
            account: account.clone(),
            password: password.to_string(),
            file,
        };
        let reopened = self
            .reopen_with(|provider| provider.open_catalog_on(&connection, true, Via::Only(shared)));
        if let Some(error) = &self.login_refusal {
            refusals.lock().expect("login refusals").record(
                key,
                &connection.account,
                &connection.password,
                error,
            );
        }
        use zeroize::Zeroize;
        connection.password.zeroize();
        reopened.map(|()| true)
    }

    fn reopen_local(&mut self, file: &om_data::LocalFile) -> Result<(), DataError> {
        // The related files' hosts stay open through the reconnect, so
        // their engines, locks and ports are the ones it opens them on
        // again: no other process can take them meanwhile.
        let related = self.external.local_leases();
        let result = self.reopen_with(|provider| {
            // The old session goes, but the private engine and its exclusive
            // file lock remain. open_local reuses this waiting host.
            if !provider
                .local
                .as_ref()
                .is_some_and(|host| same_file(host.path(), &file.path))
            {
                return Err(DataError::with_code(
                    802,
                    "The local file's engine is no longer open.",
                ));
            }
            let host = provider.local.take();
            provider.close();
            provider.local = host;
            match provider.open_local_session(file)? {
                om_data::LocalOpen::Opened { .. } => Ok(()),
                om_data::LocalOpen::NeedsLogin { .. } => Err(DataError::with_code(
                    212,
                    "An account is required to reconnect this file.",
                )),
            }
        });
        drop(related);
        result
    }

    fn record_by_id(&mut self, layout_id: u32, record_id: i64) -> Result<RecordPage, DataError> {
        let (table, rows) = self.read_record_rows(layout_id, record_id)?;
        for row in &rows {
            // A save of the record is checked against this version until
            // it is written or released, whatever is read meanwhile.
            let key = (table.id, row.id);
            let version = records::mod_count(row)?;
            self.saw_version(key, version);
            self.pinned
                .insert((self.window, key.0, key.1), Some(version));
        }
        let count = rows.len() as u64;
        record_page(rows, &table, count)
    }

    fn retain_record(&mut self, layout_id: u32, record_id: i64, version: Option<u64>) {
        let table = self
            .decoded_layout(layout_id)
            .and_then(|layout| self.read_table_for_layout(&layout));
        if let (Ok(table), Ok(id)) = (table, u64::try_from(record_id)) {
            // Carry the accepted UI page's version through the worker queue.
            // A later backend read cannot authorize this draft. An explicit
            // recovery readback may already have retained a checked baseline.
            let own = (self.window, table.id, id);
            if !table.fields.iter().any(|f| {
                f.kind == om_hbam::FieldKind::Normal && f.data_type == om_hbam::DataType::Container
            }) {
                self.unprepared_root_edits
                    .insert((self.window, table.id, record_id));
            }
            self.pinned.entry(own).or_insert(version);
        }
    }

    fn release_record(&mut self, layout_id: u32, record_id: i64) {
        let table = self
            .decoded_layout(layout_id)
            .and_then(|layout| self.read_table_for_layout(&layout));
        if let (Ok(table), Ok(id)) = (table, u64::try_from(record_id)) {
            self.unprepared_root_edits
                .remove(&(self.window, table.id, record_id));
            self.pinned.remove(&(self.window, table.id, id));
        }
    }

    fn retain_related_record(&mut self, table: u64, record: i64, version: u64) -> bool {
        let Ok(id) = u64::try_from(record) else {
            return false;
        };
        match self.pinned.entry((self.window, table, id)) {
            std::collections::btree_map::Entry::Occupied(_) => false,
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(version));
                true
            }
        }
    }

    fn release_related_record(&mut self, table: u64, record: i64) {
        if let Ok(id) = u64::try_from(record) {
            self.pinned.remove(&(self.window, table, id));
        }
    }

    fn record_position(
        &mut self,
        layout_id: u32,
        record_id: i64,
    ) -> Result<Option<u64>, DataError> {
        let layout = self.decoded_layout(layout_id)?;
        let table = self.read_table_for_layout(&layout)?;
        self.restore_saved_found(&layout, &table);
        let Ok(id) = u64::try_from(record_id) else {
            return Ok(None);
        };
        let (found, _) = self.found_ids(table.id)?;
        Ok(found.iter().position(|r| *r == id).map(|i| i as u64))
    }

    fn keep_found_set(&mut self, layout_id: u32, record: Option<i64>) {
        if self.local.is_none() {
            return;
        }
        let Ok(layout) = self.decoded_layout(layout_id) else {
            return;
        };
        let Ok(table) = self.read_table_for_layout(&layout) else {
            return;
        };
        self.keep_window_found_set(&layout, &table, record);
    }

    fn take_saved_position(&mut self, layout_id: u32) -> Option<u64> {
        // Only a local file's window restores (found.rs).
        self.local.as_ref()?;
        let layout = self.decoded_layout(layout_id).ok()?;
        let table = self.read_table_for_layout(&layout).ok()?;
        self.restore_saved_found(&layout, &table);
        let current = self.take_saved_current(table.id)?;
        let (found, _) = self.found_ids(table.id).ok()?;
        found.iter().position(|r| *r == current).map(|i| i as u64)
    }

    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        self.require_open()?;
        // In the catalog's display order `[4, 1, 03]`, as FileMaker lists
        // them (Get ( LayoutNumber ), the layout menu).
        let order = download_tree(&self.layout_downloads)
            .ok()
            .and_then(|tree| om_data::meta::display_order(&tree, &[om_data::meta::LAYOUTS]))
            .unwrap_or_default();
        let mut layouts: Vec<&om_hbam::Layout> = self.catalog.layouts.iter().collect();
        layouts.sort_by_key(|l| {
            (
                order.iter().position(|o| *o == l.id).unwrap_or(usize::MAX),
                l.id,
            )
        });
        layouts
            .into_iter()
            .filter_map(|raw| {
                let decoded = match om_layout::Layout::decode(raw) {
                    Ok(layout) => layout,
                    Err(e) => return Some(Err(failure("decode server layout", e))),
                };
                // Catalog folders and separators have no table context.
                if decoded.def.as_ref().map_or(0, |d| d.table_alias_key) == 0 {
                    return None;
                }
                Some(
                    u32::try_from(raw.id)
                        .map(|id| LayoutInfo {
                            id,
                            name: raw.name.clone(),
                        })
                        .map_err(|_| {
                            DataError::new(
                                "The server returned a layout id outside the supported range.",
                            )
                        }),
                )
            })
            .collect()
    }

    /// The layout menu, without the layouts the account may not see.
    fn layout_menu(&mut self) -> Result<Vec<om_data::LayoutMenuEntry>, DataError> {
        self.require_open()?;
        let mut menu =
            layout_menu_from_tree(&download_tree(&self.layout_downloads)?, &self.catalog)?;
        // Folders, their ends (flags 1, 2) and separators (0x10) stay, so
        // the menu keeps its nesting.
        menu.retain(|entry| {
            entry.flags & (1 | 2 | 0x10) != 0 || self.shows_layout(u64::from(entry.id))
        });
        Ok(menu)
    }

    fn records(
        &mut self,
        layout_id: u32,
        offset: u64,
        limit: u32,
    ) -> Result<RecordPage, DataError> {
        self.read_page(layout_id, offset, limit, None)
    }

    fn records_with_calculations(
        &mut self,
        layout_id: u32,
        offset: u64,
        limit: u32,
        fields: &[String],
    ) -> Result<RecordPage, DataError> {
        self.calc.library.get_mut().refresh();
        self.read_page(layout_id, offset, limit, Some(fields))
    }

    fn records_with_summary(
        &mut self,
        layout_id: u32,
        offset: u64,
        limit: u32,
        fields: &[String],
    ) -> Result<(RecordPage, Option<om_data::found::FoundSet>), DataError> {
        self.calc.library.get_mut().refresh();
        self.read_page_summarized(layout_id, offset, limit, Some(fields))
            .map(|(page, summary)| (page, Some(summary)))
    }

    fn render_layout(&mut self, layout_id: u32) -> Result<Option<RenderLayout>, DataError> {
        let mut layout = self.decoded_layout(layout_id)?;
        let field_bindings = self.bindings(&layout)?;
        // The related files it reads open before it is drawn (#983).
        let schema = self.calc_schema_for_layout(&layout)?;
        resolve_merge_bindings(&mut layout, &schema);
        Ok(Some(RenderLayout {
            layout,
            field_bindings,
        }))
    }

    fn layout_geometry(&mut self, layout_id: u32) -> Result<Option<LayoutGeometry>, DataError> {
        let layout = self.decoded_layout(layout_id)?;
        let bindings = self.bindings(&layout)?;
        let mut objects = Vec::new();
        let mut seen = BTreeSet::new();
        for id in &layout.objects.root {
            flatten_object(
                *id,
                &layout.objects,
                &bindings,
                (0.0, 0.0),
                &mut seen,
                &mut objects,
            );
        }
        // Preserve unlisted objects rather than quietly dropping them.
        for object in &layout.objects.objects {
            if object.parent.is_none() {
                flatten_object(
                    object.id,
                    &layout.objects,
                    &bindings,
                    (0.0, 0.0),
                    &mut seen,
                    &mut objects,
                );
            }
        }
        // The revision of the member this geometry was decoded from (the
        // layouts read when the file opened, or since a placement).
        let revision = download_tree(&self.layout_downloads)?
            .at(&[4, 5, u64::from(layout_id)])
            .map(|member| om_protocol::authoring::catalog_revision(&member.to_owned_node()));
        Ok(Some(LayoutGeometry {
            revision,
            width: layout.width(),
            height: layout.height(),
            objects,
            parts: layout
                .parts
                .iter()
                .map(|part| PartGeom {
                    kind_name: part_kind(part.kind()).into(),
                    top: part.top,
                    height: part.height,
                })
                .collect(),
        }))
    }

    /// A related file's connection is to the same host: losing it is
    /// losing the host (network-protocol.md 5.5, "Related files").
    fn connection_lost(&self) -> Option<DataError> {
        let session = self.session.as_ref()?;
        if let Some(reason) = session.connection_lost() {
            return Some(DataError::connection_lost(format!(
                "The connection to the host was lost ({reason})."
            )));
        }
        let (file, reason) = self.external.connection_lost()?;
        Some(DataError::connection_lost(format!(
            "The connection to the host was lost for related file “{file}” ({reason})."
        )))
    }

    fn carried_record_sessions(&self) -> Vec<u32> {
        self.carried_windows()
    }

    fn take_recreated_records(&mut self) -> Vec<om_data::RecreatedRecord> {
        std::mem::take(&mut self.recreated)
    }

    fn recreate_record(&mut self) -> Result<(), DataError> {
        self.recreate_kept()
    }

    fn related_record_reserved(&self, table: u64, record: i64) -> bool {
        self.related_reserved(table, record)
    }

    fn preview_survives_reconnect(&self, id: om_data::PreparedId) -> bool {
        self.previews.survives_reconnect(id)
    }

    fn discard_unrecreated(&mut self) -> Result<(), DataError> {
        self.discard_kept()
    }

    /// The thread's count of requests that may carry or commit record
    /// data (`om_session::record_requests_sent`): a write of this
    /// provider's, of its related files' and of the files beside it sends
    /// its requests on the caller's thread, through every connection it
    /// reaches, a file on this computer's included (its private host is
    /// reached through a session too).
    fn submission_mark(&self) -> Option<om_data::SubmissionMark> {
        Some(om_data::SubmissionMark::new(
            om_session::record_requests_sent(),
        ))
    }

    fn take_grown_schema(&mut self) -> bool {
        std::mem::take(&mut self.calc.grown)
    }

    fn related_file_warnings(&self) -> Vec<String> {
        self.external.warnings().to_vec()
    }

    fn related_sign_ins(&self) -> Vec<om_data::RelatedSignIn> {
        self.external.sign_ins()
    }

    fn sign_in_related(
        &mut self,
        file: &str,
        account: &str,
        password: &str,
    ) -> Result<(), DataError> {
        self.require_open()?;
        // A hosted file's related files are on its host; a local file's go
        // through their own private hosts, whatever host and port say.
        let (host, port) = self
            .peer_auth
            .as_ref()
            .map_or(("127.0.0.1".to_owned(), 0), |auth| {
                (auth.host.clone(), auth.port)
            });
        let mut connection = Connection {
            host,
            port,
            account: account.to_owned(),
            password: password.to_owned(),
            file: file.to_owned(),
        };
        let result = self.external.sign_in(
            &self.config,
            &self.close_requests,
            self.session.as_ref(),
            &connection,
        );
        use zeroize::Zeroize;
        connection.password.zeroize();
        result?;
        // Built again with the file; the files before it keep their ids.
        self.drop_calc_schema();
        let built = self.calc_schema_cached().map(|_| ());
        // An account another host admitted stays, for its other files a
        // script or a later read opens.
        self.external.drop_lent_credentials();
        built
    }

    fn close_requests(&self) -> Option<om_data::CloseRequests> {
        Some(self.close_requests.clone())
    }

    fn close(&mut self) {
        self.park_asset_receipt();
        self.status_download = None;
        self.recovery_creation = None;
        // A partly applied save settles while its connections work: the
        // work not applied goes and its transactions end, nothing undone.
        // On a lost connection (a reconnect) its report and freeze stay,
        // its files not known applied unresolved (#1006).
        self.close_unsettled();
        self.close_desktop_file();
        // Held transactions have not submitted op70. Release their uploads
        // before disconnecting; a submitted ambiguous commit already took
        // the transaction out and intentionally retains its references.
        if self.transaction.is_some() {
            let _ = self.revert();
        }
        for session in std::mem::take(&mut self.record_sessions).into_values() {
            self.end_parked_session(session);
        }
        self.external.close();
        // An open Manage Database's locks go first, then the connection.
        let _ = self.cancel_schema_edit();
        if let Some(session) = self.session.take() {
            let _ = session.disconnect();
        }
        // A local file's host goes with its session: the file's lock and
        // port are released before the next file opens.
        self.release_local();
        self.catalog = Catalog::default();
        self.schema_downloads.clear();
        *self.own_schema.get_mut() = None;
        self.layout_downloads.clear();
        self.account_name.clear();
        self.account_info = Default::default();
        self.peer_auth = None;
        self.local_auth = None;
        self.password_change_due = None;
        self.privileges = privileges::CatalogPrivileges::default();
        self.host_version.clear();
        self.mod_counts.clear();
        self.pinned.clear();
        self.unprepared_root_edits.clear();
        self.script_revisions.clear();
        self.script_saving = None;
        self.script_edit_base = None;
        self.calc = calc::CalcState::default();
        // The host releases a connection's locks when it disconnects.
        self.transaction = None;
        self.owner_transaction = Default::default();
        self.previews.clear();
        // Its commits' files were on the connections it closed.
        self.participants.clear();
        // A reopen takes what it carries before it closes (#152).
        self.carried.clear();
        if !self.scalar_carry.as_ref().is_some_and(|c| c.has_assets()) {
            self.scalar_carry = None;
        }
        self.asset_proofs.clear();
        self.pending_assets.clear();
        self.scalar_refusal = None;
        self.recreated.clear();
        self.unrecreated.clear();
    }
}

impl Drop for NativeProvider {
    fn drop(&mut self) {
        self.close();
    }
}

/// A refused password change (`LocalHost::change_password`) as the window
/// shows it, with FileMaker's code.
fn password_change_error(
    refusal: &om_server::hosting::PasswordChangeError,
    account: &str,
) -> DataError {
    use om_server::hosting::PasswordChangeError as Refusal;
    let message = match refusal {
        Refusal::Refused => {
            "The account name and password you entered cannot be used to access this file."
                .to_owned()
        }
        Refusal::NotAllowed => {
            "This account's privilege set does not allow changing its password.".into()
        }
        Refusal::TooShort(minimum) => {
            format!("The new password must have at least {minimum} characters.")
        }
        Refusal::Unchanged => "The new password must be different from the old one.".into(),
        Refusal::TooLong => "The new password can have at most 100 characters.".into(),
        Refusal::Inactive => format!("The account “{account}” is inactive in this file."),
        Refusal::Unwritten(reason) => format!("The new password could not be saved: {reason}."),
    };
    match refusal.code() {
        Some(code) => DataError::with_code(code, message),
        None => DataError::new(message),
    }
}

/// Key of a table's records, `[T, 05]` (file-format.md 3.4).
const RECORDS: u8 = 0x05;
/// Key of a table's record list, `[T, 0d]` (write-path.md 5.3).
const RECORD_IDS: u8 = 0x0d;

struct Download {
    path: Vec<Vec<u8>>,
    slots: Vec<u8>,
}

/// The theme catalog `[6]` (layout-model.md 7.5).
const THEMES: u64 = 6;
/// The file's node `[2]`: its locale, options and `mInternalCreation`
/// (file-metadata.md 3).
const STATUS: u64 = 2;

fn key_bytes(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    bytes[bytes.iter().position(|b| *b != 0).unwrap_or(7)..].to_vec()
}

/// Download the node at `path`, which must exist.
fn download(session: &mut Session, path: &[u64]) -> Result<Download, DataError> {
    download_node(session, path.iter().map(|k| key_bytes(*k)).collect())?
        .ok_or_else(|| DataError::new(format!("The server has no {path:?}.")))
}

/// Whether the server has the key `path`, a leaf or a node (op90).
fn key_exists(session: &mut Session, path: Vec<Vec<u8>>) -> Result<bool, DataError> {
    let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let reply = session
        .download(&refs)
        .map_err(|e| failure(&format!("download server path {path:02x?}"), e))?;
    if reply.lock_error != 0 {
        return Err(DataError::new(format!(
            "Server download {path:02x?} failed (lock error 0x{:04x}).",
            reply.lock_error
        )));
    }
    Ok(reply.node_state != NodeState::Missing)
}

/// Download the node at `path` (op90), or `None` when the server has no
/// such key.
fn download_node(session: &mut Session, path: Vec<Vec<u8>>) -> Result<Option<Download>, DataError> {
    let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let reply = session
        .download(&refs)
        .map_err(|e| failure(&format!("download server path {path:02x?}"), e))?;
    node_from_reply(path, reply)
}

/// The node at `path` from `reply`, its op90 download, checked as
/// [`download_node`] checks one: `None` when the server has no such key.
fn node_from_reply(
    path: Vec<Vec<u8>>,
    reply: om_protocol::DownloadReply,
) -> Result<Option<Download>, DataError> {
    if reply.lock_error != 0 {
        return Err(DataError::new(format!(
            "Server download {path:02x?} failed (lock error 0x{:04x}).",
            reply.lock_error
        )));
    }
    match reply.node_state {
        NodeState::Missing => return Ok(None),
        NodeState::NonTerminal => {}
        state => {
            return Err(DataError::new(format!(
                "Server path {path:02x?} is not a node ({state:?})."
            )))
        }
    }
    let data = Download {
        path,
        slots: reply.slots,
    };
    validate_download(&data)?;
    Ok(Some(data))
}

/// The font families of the file whose catalogs `downloads` holds: its
/// font catalog `[25]`, read at open with the layout catalogs (#1098).
fn file_fonts(downloads: &[Download]) -> Result<Arc<BTreeMap<u64, String>>, DataError> {
    if downloads.is_empty() {
        return Ok(Arc::default());
    }
    Ok(Arc::new(om_data::rules::schema::font_families(
        &download_tree(downloads)?,
    )))
}

/// The root file's layouts as its layout catalog was downloaded: each
/// member read whole, and the index's entry of each other one (on demand,
/// #1019), in id order.
fn catalog_layouts(downloads: &[Download]) -> Result<Vec<om_hbam::Layout>, DataError> {
    if downloads.is_empty() {
        return Ok(Vec::new());
    }
    let tree = download_tree(downloads)?;
    let mut layouts =
        om_hbam::catalog::layouts(&tree).map_err(|e| failure("decode server layouts", e))?;
    let read: BTreeSet<u64> = layouts.iter().map(|l| l.id).collect();
    let indexed = om_hbam::catalog::layout_index(&tree)
        .map_err(|e| failure("decode the server's layout index", e))?;
    layouts.extend(indexed.into_iter().filter(|l| !read.contains(&l.id)));
    layouts.sort_by_key(|l| l.id);
    Ok(layouts)
}

/// Every record id of base table `table`, from its record list `[T, 0d]`
/// (write-path.md 5.3). A table that never had a record has none.
fn record_ids(session: &mut Session, table: u64) -> Result<RidList, DataError> {
    let path = vec![key_bytes(table), vec![RECORD_IDS]];
    let Some(data) = download_node(session, path)? else {
        return Ok(RidList::new());
    };
    let tree = download_tree(std::slice::from_ref(&data))?;
    let refs: Vec<&[u8]> = data.path.iter().map(Vec::as_slice).collect();
    tree.node(&refs)
        .map(RidList::from_node)
        .transpose()
        .map_err(|e| failure("decode the server's record list", e))
        .map(Option::unwrap_or_default)
}

/// Whether base table `table` holds no record: its record list `[T, 0d]`
/// names none and nothing is stored under `[T, 05]`. Both are read, since a
/// damaged file could keep records its list does not name.
pub(crate) fn holds_no_record(session: &mut Session, table: u64) -> Result<bool, DataError> {
    if !record_ids(session, table)?.is_empty() {
        return Ok(false);
    }
    let path = vec![key_bytes(table), vec![RECORDS]];
    let Some(data) = download_node(session, path)? else {
        return Ok(true);
    };
    let tree = download_tree(std::slice::from_ref(&data))?;
    let refs: Vec<&[u8]> = data.path.iter().map(Vec::as_slice).collect();
    Ok(tree.node(&refs).is_none_or(|node| node.children.is_empty()))
}

/// The records `ids` of base table `table` that exist, in one op92
/// (network-protocol.md 5.6.6b), relative to `[T, 05]`. The keys go in key
/// order whatever the order of `ids` (a sorted found set): the host fails
/// a list out of order.
fn download_records(session: &mut Session, table: u64, ids: &[u64]) -> Result<Download, DataError> {
    let path = vec![key_bytes(table), vec![RECORDS]];
    if ids.is_empty() {
        return Ok(Download {
            path,
            slots: vec![om_hbam::slot::BEGIN, om_hbam::slot::POP],
        });
    }
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let keys: Vec<Vec<u8>> = ids.iter().map(|id| serial::encode(*id)).collect();
    let key_refs: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
    let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let reply = session
        .download_list(&refs, &key_refs, 0)
        .map_err(|e| failure("download server records", e))?;
    if reply.status != 0 {
        return Err(DataError::new(format!(
            "Server record download failed (lock error 0x{:04x}).",
            reply.status
        )));
    }
    let data = Download {
        path,
        slots: reply.slots,
    };
    validate_download(&data)?;
    Ok(data)
}

fn validate_download(data: &Download) -> Result<(), DataError> {
    let mut reader = SlotReader::with_path(
        &data.slots,
        data.path.iter().map(|k| KeySegment(k)).collect(),
    );
    for slot in reader.by_ref() {
        slot.map_err(|e| failure("decode server download", e))?;
    }
    if reader.root_closed() || reader.path().len() != data.path.len().saturating_sub(1) {
        return Err(DataError::new(
            "The server download is incomplete or has an unsupported continuation. No partial \
             records were loaded.",
        ));
    }
    Ok(())
}

fn download_tree_refs<'a>(downloads: &[&'a Download]) -> Result<KeyTree<'a>, DataError> {
    KeyTree::from_readers(
        downloads.iter().map(|d| {
            SlotReader::with_path(&d.slots, d.path.iter().map(|k| KeySegment(k)).collect())
        }),
    )
    .map_err(|e| failure("decode server HBAM tree", e))
}

/// The table occurrence a layout shows.
fn layout_occurrence(layout: &om_layout::Layout) -> Result<u64, DataError> {
    occurrence_id(layout.def.as_ref().map_or(0, |d| d.table_alias_key))
        .ok_or_else(|| DataError::new("The layout has no table occurrence."))
}

fn download_tree(downloads: &[Download]) -> Result<KeyTree<'_>, DataError> {
    KeyTree::from_readers(
        downloads.iter().map(|d| {
            SlotReader::with_path(&d.slots, d.path.iter().map(|k| KeySegment(k)).collect())
        }),
    )
    .map_err(|e| failure("decode server HBAM tree", e))
}

/// The id of the table occurrence a layout names by `alias`. Layouts store
/// the serial-key value of the occurrence's key (layout-model.md 4.1), the
/// catalog the key's bytes: alias 0x104082 is occurrence `d0 00 02`. `None`
/// when no catalog id has that key.
fn occurrence_id(alias: u64) -> Option<u64> {
    om_hbam::slot::be_uint(&serial::encode(alias))
}

/// Whether `occurrence` shows a related file's table: an occurrence of
/// another file's table, resolved or not, or one bound to a table imported
/// from a related file (external.rs).
fn shows_related_file(schema: &om_data::host::CalcSchema, occurrence: u64) -> bool {
    schema.data_source(occurrence).is_some()
        || schema
            .table_of(occurrence)
            .is_some_and(|table| schema.table_context(table.id).is_some())
}

/// Error 3 for an operation not yet supported on another file's table.
/// Layout browsing and ordinary record creation, edits and deletion use
/// owning routes; other writes and index-based layout operations retain
/// local-only checks.
fn related_file_occurrence(name: &str) -> DataError {
    DataError::with_code(
        om_data::found::code::UNSUPPORTED,
        format!(
            "Table occurrence “{name}” shows a table of a related file. OpenMaker reads related \
             files' tables and browses their layouts; value lists and changes on them are \
             not supported yet."
        ),
    )
}

/// The base table of the occurrence a layout shows. An unknown occurrence
/// or definition fails: selecting another table would display unrelated
/// data.
fn table_for_alias(catalog: &Catalog, alias: u64) -> Result<&Table, DataError> {
    let occurrence = occurrence_id(alias)
        .and_then(|id| catalog.table_occurrences.iter().find(|o| o.id == id))
        .ok_or_else(|| {
            DataError::new(format!(
                "The server schema has no table occurrence for layout context 0x{alias:x}."
            ))
        })?;
    let table_id = match occurrence.reference() {
        Some((0, table)) => table,
        Some(_) => return Err(related_file_occurrence(&occurrence.name)),
        None => {
            return Err(DataError::new(format!(
                "Unsupported definition of table occurrence {}.",
                occurrence.name
            )))
        }
    };
    catalog.table(table_id).ok_or_else(|| {
        DataError::new(format!(
            "The layout references inaccessible base table 0x{table_id:x}."
        ))
    })
}

/// Name every merge field in `layout`'s object text: `Field` in the layout's
/// own occurrence, `Occurrence::Field` in any other (layout-model.md 4.1).
/// Merge references name the occurrence and field by their raw catalog keys.
/// An object with a reference the hydrated schema cannot name keeps its stored text.
fn resolve_merge_bindings(layout: &mut om_layout::Layout, schema: &om_data::host::CalcSchema) {
    let current = occurrence_id(layout.def.as_ref().map_or(0, |def| def.table_alias_key));
    for object in &mut layout.objects.objects {
        let specs = object.merge_field_specs();
        if specs.is_empty() {
            continue;
        }
        let names: Option<Vec<_>> = specs
            .iter()
            .map(|spec| {
                let occurrence = schema.occurrence(spec.table_key)?;
                let field = schema.field(spec.table_key, spec.field_key)?;
                Some(if Some(occurrence.id) == current {
                    field.name.clone()
                } else {
                    format!("{}::{}", occurrence.name, field.name)
                })
            })
            .collect();
        if let Some(names) = names {
            object.resolve_merge_fields(&names);
        }
    }
}

/// A stored cell as a record page shows it. Text, numbers, dates, times
/// and timestamps keep their text when it is stored with character styles,
/// as a chunked value beside a style subtree `ff` (calc-values.md,
/// implementation notes), which `Cell::display` would show as `<nested>`.
/// Any other nested form of these types shows nothing; containers keep
/// their display form.
fn page_value(cell: &om_hbam::Cell, data_type: om_hbam::DataType) -> Option<String> {
    use om_hbam::DataType::*;
    match data_type {
        Text | Number | Date | Time | Timestamp => om_layout::StyledText::from_cell(cell)
            .map(|text| text.text)
            .or_else(|| calc::cell_text(cell))
            .or_else(|| match cell {
                om_hbam::Cell::Value(_) => Some(cell.display()),
                om_hbam::Cell::Nested(_) => None,
            }),
        Container | Unknown(_) => Some(cell.display()),
    }
}

/// A stored record's values as a record page shows them, one per field of
/// `table` ([`page_value`]): what pages, and the records a write returns,
/// hold.
fn page_values(row: &om_hbam::Record, table: &Table) -> Vec<Option<String>> {
    table
        .fields
        .iter()
        .map(|field| page_value(row.cells.get(&field.id)?, field.data_type))
        .collect()
}

fn record_page(
    rows: Vec<om_hbam::Record>,
    table: &Table,
    total: u64,
) -> Result<RecordPage, DataError> {
    let mut versions = BTreeMap::new();
    let records = rows
        .into_iter()
        .map(|row| {
            let id = i64::try_from(row.id)
                .map_err(|_| DataError::new("A server record id exceeds the supported range."))?;
            versions.insert(id, records::mod_count(&row)?);
            Ok(Record {
                id,
                values: page_values(&row, table),
            })
        })
        .collect::<Result<_, DataError>>()?;
    let fields = table
        .fields
        .iter()
        .map(|field| Field {
            name: field.name.clone(),
            kind: match field.data_type {
                om_hbam::DataType::Number => FieldKind::Number,
                om_hbam::DataType::Date => FieldKind::Date,
                om_hbam::DataType::Time => FieldKind::Time,
                om_hbam::DataType::Timestamp => FieldKind::Timestamp,
                om_hbam::DataType::Container => FieldKind::Container,
                _ => FieldKind::Text,
            },
        })
        .collect();
    Ok(RecordPage {
        versions,
        fields,
        records,
        total,
    })
}

fn flatten_object(
    id: u64,
    store: &om_layout::ObjectStore,
    bindings: &BTreeMap<u64, String>,
    offset: (f64, f64),
    seen: &mut BTreeSet<u64>,
    output: &mut Vec<ObjectGeom>,
) {
    if !seen.insert(id) {
        return;
    }
    let Some(object) = store.get(id) else {
        return;
    };
    let mut child_offset = offset;
    if let Some(rect) = &object.bounds {
        child_offset = (offset.0 + rect.left, offset.1 + rect.top);
        output.push(ObjectGeom {
            kind_name: object_kind(object.kind()),
            rect: GeomRect {
                top: rect.top + offset.1,
                left: rect.left + offset.0,
                bottom: rect.bottom + offset.1,
                right: rect.right + offset.0,
            },
            field_name: bindings.get(&id).cloned(),
            key: id,
            movable: om_data::layout_objects::movable(store, id).is_ok(),
            deletable: om_data::layout_objects::deletable(store, id).is_ok(),
        });
    }
    for child in &object.children {
        flatten_object(*child, store, bindings, child_offset, seen, output);
    }
}

/// An object's type as FileMaker Pro names it in the Objects pane and
/// Inspector.
fn object_kind(kind: om_layout::ObjectKind) -> String {
    use om_layout::ObjectKind::*;
    match kind {
        Field => "Field",
        Text => "Text",
        Graphic => "Graphic",
        Line => "Line",
        Rectangle => "Rectangle",
        RoundedRectangle => "Rounded Rectangle",
        Oval => "Oval",
        Group => "Group",
        Portal => "Portal",
        Button => "Button",
        TabControl => "Tab Control",
        TabPanel => "Tab Panel",
        // Charts and plug-in objects share this type; web viewers are the
        // common case.
        External => "Web Viewer",
        PopoverButton => "Popover Button",
        Popover => "Popover",
        SlideControl => "Slide Control",
        SlidePanel => "Slide Panel",
        ButtonBar => "Button Bar",
        Unknown(code) => return format!("Object type {code}"),
    }
    .into()
}

fn part_kind(kind: om_layout::PartKind) -> &'static str {
    use om_layout::PartKind::*;
    match kind {
        TitleHeader => "Title Header",
        Header => "Header",
        LeadingGrandSummary => "Leading Grand Summary",
        LeadingSubSummary => "Leading Sub-summary",
        Body => "Body",
        TrailingSubSummary => "Trailing Sub-summary",
        TrailingGrandSummary => "Trailing Grand Summary",
        Footer => "Footer",
        TitleFooter => "Title Footer",
        TopNavigation => "Top Navigation",
        BottomNavigation => "Bottom Navigation",
        Other(_) => "Part",
    }
}

/// Preserve every display-order member; only the GUI interprets menu visibility.
fn layout_menu_from_tree(
    tree: &om_hbam::KeyTree<'_>,
    catalog: &om_hbam::Catalog,
) -> Result<Vec<om_data::LayoutMenuEntry>, DataError> {
    let order = om_data::meta::display_order(tree, &[om_data::meta::LAYOUTS]).unwrap_or_default();
    let mut layouts: Vec<_> = catalog.layouts.iter().collect();
    layouts.sort_by_key(|l| {
        (
            order
                .iter()
                .position(|id| *id == l.id)
                .unwrap_or(usize::MAX),
            l.id,
        )
    });
    layouts
        .into_iter()
        .map(|l| {
            Ok(om_data::LayoutMenuEntry {
                id: u32::try_from(l.id).map_err(|_| DataError::new("Unsupported layout id."))?,
                name: l.name.clone(),
                flags: om_data::meta::layout_flags(tree, l.id),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::slot::{encode_chunk, minimal_code};

    fn leaf(key: &[u8], value: &[u8]) -> Vec<u8> {
        encode_chunk(
            minimal_code(false, key.len(), Some(value.len())).unwrap(),
            key,
            value,
        )
        .unwrap()
    }

    fn push(key: &[u8]) -> Vec<u8> {
        encode_chunk(minimal_code(true, key.len(), None).unwrap(), key, &[]).unwrap()
    }

    fn catalog() -> Catalog {
        Catalog {
            tables: vec![Table {
                id: 0x8002,
                name: "People".into(),
                fields: vec![
                    om_hbam::Field {
                        id: 1,
                        name: "Name".into(),
                        kind: om_hbam::FieldKind::Normal,
                        data_type: om_hbam::DataType::Text,
                        options: vec![1, 1],
                    },
                    om_hbam::Field {
                        id: 2,
                        name: "Score".into(),
                        kind: om_hbam::FieldKind::Normal,
                        data_type: om_hbam::DataType::Number,
                        options: vec![1, 2],
                    },
                ],
            }],
            table_occurrences: vec![om_hbam::TableOccurrence {
                id: 0xd00002,
                name: "People".into(),
                definition: vec![2, 1, 0, 3, 2, 0x80, 2],
            }],
            layouts: vec![],
        }
    }

    #[test]
    fn layout_mapping_requires_matching_occurrence_and_base_table() {
        let mut schema = catalog();
        // Layouts name occurrence d0 00 02 by its serial-key value.
        assert_eq!(occurrence_id(0x10_4082), Some(0xd0_0002));
        assert_eq!(table_for_alias(&schema, 0x10_4082).unwrap().id, 0x8002);
        assert!(table_for_alias(&schema, 0x10_4083).is_err());
        assert!(table_for_alias(&schema, 0).is_err());
        assert!(table_for_alias(&schema, 0x20_4082).is_err());
        schema.table_occurrences[0].definition[0] = 3;
        assert!(table_for_alias(&schema, 0x10_4082).is_err());
        schema.table_occurrences[0].definition = vec![2, 1, 0, 3, 2, 0x80, 3];
        assert!(table_for_alias(&schema, 0x10_4082).is_err());
        schema.table_occurrences[0].definition.truncate(6);
        assert!(table_for_alias(&schema, 0x10_4082).is_err());
    }

    #[test]
    fn occurrences_past_d0_00_7f_map_to_their_base_table() {
        let mut schema = catalog();
        schema.tables.push(Table {
            id: 0x8003,
            name: "Later".into(),
            fields: vec![],
        });
        for (occurrence, alias) in [(0xd0_0080, 0x10_4100), (0xd0_1234, 0x10_52b4)] {
            schema.table_occurrences.push(om_hbam::TableOccurrence {
                id: occurrence,
                name: format!("Occurrence {occurrence:x}"),
                definition: vec![2, 1, 0, 3, 2, 0x80, 3, 0, 0],
            });
            assert_eq!(occurrence_id(alias), Some(occurrence));
            assert_eq!(table_for_alias(&schema, alias).unwrap().id, 0x8003);
        }
        // 0x10_4100 is not d0 00 02 (the low seven bits alone would say so).
        assert_eq!(table_for_alias(&schema, 0x10_4082).unwrap().id, 0x8002);
    }

    #[test]
    fn page_downloads_keep_context_and_serial_record_ids() {
        // The records an op92 returns for ids 2, 128 and 538, relative to
        // [80 02, 05]; these record keys differ from big-endian numbers.
        let mut slots = vec![om_hbam::slot::BEGIN];
        for (key, name, version) in [
            (&[0x02][..], "Ada", None),
            (&[0x80, 0x00][..], "Boundary", Some(2)),
            (&[0x81, 0x9a][..], "After", Some(3)),
        ] {
            slots.extend(push(key));
            slots.push(om_hbam::slot::BEGIN);
            slots.extend(leaf(&[1], &om_scsu::encode_scsu(name)));
            if let Some(version) = version {
                slots.extend(leaf(&[0xfc], &[1, version]));
            }
            slots.push(om_hbam::slot::POP);
        }
        slots.push(om_hbam::slot::POP);
        let data = Download {
            path: vec![vec![0x80, 2], vec![5]],
            slots,
        };
        validate_download(&data).unwrap();
        let tree = download_tree(std::slice::from_ref(&data)).unwrap();
        assert!(tree.at(&[0x8002, 5, 2, 1]).is_some());
        assert!(tree.at(&[5, 2, 1]).is_none());
        let schema = catalog();
        let table = &schema.tables[0];
        let rows = om_hbam::record::records(&tree, table.id);
        let counts: Vec<u64> = rows
            .iter()
            .map(|r| records::mod_count(r).unwrap())
            .collect();
        assert_eq!(counts, [0, 2, 3]);
        let page = record_page(rows, table, 10).unwrap();
        assert_eq!(page.versions, [(2, 0), (128, 2), (538, 3)].into());
        assert_eq!(page.total, 10);
        assert_eq!(
            page.records.iter().map(|row| row.id).collect::<Vec<_>>(),
            [2, 128, 538]
        );
        assert_eq!(page.records[1].values, vec![Some("Boundary".into()), None]);
        assert_eq!(page.fields[1].kind, FieldKind::Number);
    }

    #[test]
    fn record_pages_show_text_stored_with_character_styles() {
        use om_hbam::{Cell, DataType};
        // Text stored with a style run: header (byte and character counts),
        // one piece and the style subtree `ff`.
        let styled = |text: &str| {
            let bytes = om_scsu::encode_scsu(text);
            let count = text.chars().count() as u32;
            Cell::Nested(vec![
                (
                    vec![vec![0]],
                    Some([(bytes.len() as u32).to_be_bytes(), count.to_be_bytes()].concat()),
                ),
                (vec![vec![1]], Some(bytes)),
                (vec![vec![0xff]], None),
                (vec![vec![0xff], vec![1]], Some(vec![0, 0, 0, 7])),
            ])
        };
        let cells = [
            ("Notes", DataType::Text, styled("First line\rSecond line")),
            ("Amount", DataType::Number, styled("12.50")),
            (
                "Due",
                DataType::Date,
                Cell::Value(om_scsu::encode_scsu("10/31/2026")),
            ),
            ("Start", DataType::Time, styled("9:30:00")),
            ("Stamp", DataType::Timestamp, styled("10/31/2026 9:30:00")),
            ("Photo", DataType::Container, styled("photo.png")),
            (
                "Unknown form",
                DataType::Text,
                Cell::Nested(vec![(vec![vec![0xff]], None)]),
            ),
        ];
        let table = Table {
            id: 0x8002,
            name: "Notes".into(),
            fields: cells
                .iter()
                .zip(1..)
                .map(|((name, data_type, _), id)| om_hbam::Field {
                    id,
                    name: (*name).into(),
                    kind: om_hbam::FieldKind::Normal,
                    data_type: *data_type,
                    options: vec![1, 1],
                })
                .collect(),
        };
        let container = cells[5].2.display();
        let row = om_hbam::Record {
            id: 1,
            cells: cells
                .into_iter()
                .zip(1..)
                .map(|((.., cell), id)| (id, cell))
                .collect(),
        };
        let page = record_page(vec![row], &table, 1).unwrap();
        assert_eq!(
            page.records[0].values,
            [
                Some("First line\rSecond line".into()),
                Some("12.50".into()),
                Some("10/31/2026".into()),
                Some("9:30:00".into()),
                Some("10/31/2026 9:30:00".into()),
                // Containers keep their display form.
                Some(container),
                // An unrecognized structure is not shown as `<nested>`.
                None,
            ]
        );
    }

    #[test]
    fn incomplete_download_is_never_presented_as_a_complete_found_set() {
        let data = Download {
            path: vec![vec![0x80, 2], vec![5]],
            slots: push(&[2]),
        };
        assert!(validate_download(&data).is_err());
        let data = Download {
            path: vec![vec![4]],
            slots: vec![0x40, 0x40],
        };
        assert!(validate_download(&data).is_err());
    }

    #[test]
    fn related_occurrences_and_repetitions_do_not_bind_to_current_row() {
        let mut provider = NativeProvider::new(NativeConfig {
            client_cert_pem: vec![],
            client_key_pem: vec![],
            additional_ca_pem: vec![],
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        });
        provider.catalog = catalog();
        provider.catalog.tables[0].fields.push(om_hbam::Field {
            id: 0x80ac, // layout serial field key 300
            name: "High field".into(),
            kind: om_hbam::FieldKind::Normal,
            data_type: om_hbam::DataType::Text,
            options: vec![1, 1],
        });
        let make = |id, alias, repetition, field_key| om_layout::LayoutObject {
            id,
            object_type: 1,
            field_obj: Some(om_layout::FieldObject {
                field_spec: Some(om_layout::FieldSpec {
                    table_key: alias,
                    field_key,
                    repetition,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let layout = om_layout::Layout {
            def: Some(om_layout::LayoutDef {
                table_alias_key: 0x104082,
                ..Default::default()
            }),
            objects: om_layout::ObjectStore {
                objects: vec![
                    make(1, 0x104082, 1, 1),
                    make(2, 0x104083, 1, 1),
                    make(3, 0x104082, 2, 1),
                    make(4, 0x104082, 1, 300),
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            provider.bindings(&layout).unwrap(),
            BTreeMap::from([(1, "Name".into()), (4, "High field".into())])
        );
    }

    #[test]
    fn merge_fields_name_fields_of_related_occurrences() {
        let mut catalog = catalog();
        let field = |id, name: &str| om_hbam::Field {
            id,
            name: name.into(),
            kind: om_hbam::FieldKind::Normal,
            data_type: om_hbam::DataType::Number,
            options: vec![1, 2],
        };
        catalog.tables[0].fields.push(field(0x8123, "Length"));
        catalog.tables.push(Table {
            id: 0x8003,
            name: "Visits".into(),
            fields: vec![field(0x8123, "Quantity")],
        });
        for (id, name, table) in [
            (0xd00003, "Visits of person", 3),
            (0xd00004, "Other people", 2),
        ] {
            catalog.table_occurrences.push(om_hbam::TableOccurrence {
                id,
                name: name.into(),
                definition: vec![2, 1, 0, 3, 2, 0x80, table],
            });
        }
        let text = |text: &str| om_layout::LayoutObject {
            text: Some(text.into()),
            ..Default::default()
        };
        let mut layout = om_layout::Layout {
            def: Some(om_layout::LayoutDef {
                table_alias_key: 0x104082,
                ..Default::default()
            }),
            objects: om_layout::ObjectStore {
                objects: vec![
                    // The same raw field key names a different field in each
                    // base table; a second occurrence of the layout's own
                    // table is still qualified.
                    text(
                        "<<\u{1}\u{1}\u{10}0003D00002028123>> long, \
                         <<\u{1}\u{2}\u{10}0003D00003028123>> visits, \
                         <<\u{1}\u{1}\u{e}0003D000040101>>",
                    ),
                    text("<<\u{1}\u{1}\u{e}0003D000020101>> and <<\u{1}\u{1}\u{e}0003D000090101>>"),
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let unresolved = layout.objects.objects[1].clone();
        let mut schema = om_data::host::CalcSchema::new("", om_data::calc::Locale::en_us());
        for table in &catalog.tables {
            schema.add_table(table.id, table.name.clone());
            for field in &table.fields {
                schema.add_field(
                    table.id,
                    om_data::host::FieldDef::data(
                        field.id,
                        field.name.clone(),
                        om_data::calc::ValueType::Text,
                    ),
                );
            }
        }
        for occurrence in &catalog.table_occurrences {
            schema.add_occurrence(
                occurrence.id,
                occurrence.name.clone(),
                occurrence.base_table().unwrap(),
            );
        }
        resolve_merge_bindings(&mut layout, &schema);
        let object = &layout.objects.objects[0];
        assert_eq!(
            object.text.as_deref(),
            Some("<<Length>> long, <<Visits of person::Quantity>> visits, <<Other people::Name>>")
        );
        assert_eq!(
            object.text_runs,
            [
                om_layout::TextRun::MergeField("Length".into()),
                om_layout::TextRun::Literal(" long, ".into()),
                om_layout::TextRun::MergeField("Visits of person::Quantity".into()),
                om_layout::TextRun::Literal(" visits, ".into()),
                om_layout::TextRun::MergeField("Other people::Name".into()),
            ]
        );
        // An unknown occurrence leaves the whole object as stored.
        assert_eq!(layout.objects.objects[1], unresolved);
    }
}

#[cfg(test)]
mod layout_menu_tests {
    use super::*;
    use om_hbam::OwnedNode;

    #[test]
    fn catalog_menu_preserves_flags_order_and_index_fallback() {
        let names = ["Visible", "Hidden", "Folder", "--", "-"];
        let flags = [0u64, 0x1_0000_0008, 0x0d, 2, 0x10];
        let members = names
            .iter()
            .zip(flags)
            .enumerate()
            .map(|(i, (name, flags))| {
                let mut leaves = vec![OwnedNode::leaf([0x10], om_scsu::encode_scsu(name))];
                if i != 4 {
                    leaves.push(OwnedNode::leaf([0xf0], flags.to_be_bytes().to_vec()));
                }
                OwnedNode::list([i as u8 + 1], leaves)
            })
            .collect();
        let order = OwnedNode::list(
            [3],
            [3u8, 1, 5, 2, 4]
                .iter()
                .enumerate()
                .map(|(i, id)| OwnedNode::leaf(vec![0, 0, 0, i as u8 + 1], vec![1, *id]))
                .collect(),
        );
        let wire = OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::list(
                [4],
                vec![
                    OwnedNode::list(
                        [1],
                        vec![
                            order,
                            OwnedNode::list(
                                [7],
                                vec![OwnedNode::list(
                                    [5],
                                    vec![OwnedNode::leaf([0xf0], 0x10u64.to_be_bytes().to_vec())],
                                )],
                            ),
                        ],
                    ),
                    OwnedNode::list([5], members),
                ],
            )],
        )
        .to_wire()
        .unwrap();
        let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
        let catalog = Catalog::from_tree(&tree).unwrap();
        let menu = layout_menu_from_tree(&tree, &catalog).unwrap();
        assert_eq!(
            menu.iter().map(|e| (e.id, e.flags)).collect::<Vec<_>>(),
            [(3, 0x0d), (1, 0), (5, 0x10), (2, 0x1_0000_0008), (4, 2)]
        );
        assert_eq!(menu[0].name, "Folder");
        let schema = om_data::rules::schema::calc_schema(&tree, &catalog, "Synthetic");
        // LayoutNames includes hidden layouts and separators, excluding only
        // folder delimiters; menu presentation must not alter this list.
        assert_eq!(
            schema
                .layouts()
                .iter()
                .filter(|l| l.listed())
                .map(|l| l.id)
                .collect::<Vec<_>>(),
            [1, 5, 2]
        );
    }
}
