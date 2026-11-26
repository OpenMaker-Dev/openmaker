//! Related hosted files: the files a file's table occurrences show tables
//! of, through its external data sources (file-metadata.md 1.1).
//!
//! When the calculation schema is built, every data source's paths are
//! tried in their order, each on the host and port it names: a relative
//! path on those of the file whose data source it is, an `fmnet` path on
//! its own (#976). The first file a host has is the data source's file
//! ([`ExternalFiles::hydrate`]). A file on the opened file's host and port
//! is opened on that file's connection (network-protocol.md 5.5, "Related
//! files"): Open and Login with the same account. A file on another host or
//! port is opened only once the user signs in to it there (below), on a
//! verified TLS connection of its own to that host and port. Its catalogs are read once and its calculation schema is
//! imported into the opened file's under client-only ids
//! ([`CalcSchema::import_file_schema`]). Records, record lists, globals and
//! index finds of its tables are then read when calculations or layout pages need them, on
//! its connection and under its own table ids ([`ExternalFiles::route`],
//! [`ExternalFiles::with_session`]); no record is read while the files are
//! opened. A related file's own data sources are followed the same way,
//! breadth first, so a file several files reference opens once and a cycle
//! back to an open file ends there.
//!
//! Credentials: the account and password are kept, the password zeroed
//! when dropped, only from `open` until the related files are opened (the
//! first calculation schema, which the window asks for right after it
//! opens a file) or the file is closed, whichever comes first. They are only
//! sent to the host and port the user opened, which an `fmnet` path names
//! by the same text in any case and port
//! ([`om_data::file_refs::FilePath::fmnet_endpoint`]); nothing is looked up
//! to compare them. A file on another host or port is sent, on a verified
//! connection of its own to that host and port, an account that host
//! admitted in this session, else these credentials once for that host
//! (network-protocol.md 5.5): after any file there refused them, none is
//! sent them again, and the user signs in to the file
//! ([`ExternalFiles::sign_ins`], named by its `fmnet` path). An account a
//! host admitted goes to that host and port only, opens its other files
//! there (in the first one's session), and stays until the file closes or
//! reconnects, for the files there a script or a later read opens. A data
//! source whose search reaches a refused file ends there, as at a file
//! that refused the account, rather than binding a later path's file.
//! A file on the opened file's host first reached when the schema is built
//! again opens with the account the provider keeps for the file's scripts'
//! opens ([`ExternalFiles::lend`], #978); after a reconnect, which keeps no
//! password, the file has to be opened again. Files are told apart by host, port and
//! name, so one name on two hosts is two files.
//!
//! Files on this computer ([`ExternalFiles::for_local_file`]): a related
//! file is beside the file that names it, in its folder, by the data
//! source's relative alternatives in their order (the first that names a
//! file there; a file's own related files are beside it). Each is opened
//! through its host in the window's family (local.rs), shared with the
//! family's other sessions of that file, and logged in to with the same
//! account, which that file's own accounts must admit as they admit File >
//! Open. Files are told apart by path, links followed; a cycle back to an
//! open file ends there by path, not by name. No server is contacted and
//! no license read. Folders, volumes, `fmnet` and other forms are not
//! followed from a local file (their occurrences stay unresolved).
//!
//! Failures: a related file that cannot be opened (not hosted, no access,
//! the account refused) is tried once per open and reported in
//! [`ExternalFiles::warnings`]; at most 128 files are tried, opened or not.
//! A path the host answers it does not have sends the data source on to
//! its next path; any other failure ends its search, so a file that
//! refused the account is never sent a second Login in one open.
//! Their occurrences stay unresolved, so what
//! reads them shows `?`; they never fall back to a table of another file
//! with the same id. A related connection lost later counts as losing the
//! host ([`ExternalFiles::connection_lost`]). Closing the file while its
//! related files are being opened cancels further work after the current
//! network operation returns: the files
//! being opened stop before their Login, no further file is tried, and the
//! credentials and every related connection are dropped. The provider
//! reads through owning routes and reuses each owning provider for ordinary
//! record creation, edits and deletion. Unsupported writes retain their
//! preflight guards.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use om_data::host::{CalcSchema, ImportedFileMap};
use om_data::{CloseRequests, Connection, DataError};
use om_hbam::{Catalog, KeyTree, Node, SlotReader};
use om_session::Session;
use zeroize::Zeroizing;

use crate::local::{Lease, LocalHosts};
use crate::{
    download, download_node, download_tree, failure, key_bytes, node_from_reply, Download,
    NativeConfig,
};

/// External data sources `[32, 5, D]`, with a source's name at `10` and its
/// file reference at `82` (file-metadata.md 1.1).
const DATA_SOURCES: u64 = 0x20;
const SOURCE_NAME: u64 = 0x10;
const FILE_REFERENCE: u64 = 0x82;
/// Sibling root catalogs a related file's calculations need: tables `[3]`,
/// custom functions `[16]`, data sources `[32]` and value lists `[33]`.
/// Locale and options `[2]` reuse the login challenge when available.
/// Not its layouts, scripts or images, nor its fonts `[25]`, which
/// only styled text of its records needs: its owner provider reads them
/// when such text first names a font ([`ExternalFiles::fonts`], #1098).
const RELATED_CATALOGS: [u64; 4] = [3, 16, DATA_SOURCES, 0x21];
/// The most related files one open file tries to open, opened or not:
/// every Open counts, each path of a data source tried included.
pub(crate) const MAX_FILES: usize = 128;
/// The most data sources one open queues: each names a file, a list of
/// paths, or a file already open (which needs no Open). A bound on what
/// a file's catalogs make the client hold, not on what it opens.
const MAX_SOURCES: usize = 4096;
/// The most warnings kept: the window shows them in one alert.
const MAX_WARNINGS: usize = 20;
/// Related files opened at the same time, each on a connection of its own.
/// On the opened file's connection they open one at a time: it carries one
/// request at a time anyway (#772), and a loader waiting behind another's
/// reply would send its next request, a Login above all, after the window
/// had closed the file.
const PARALLEL_LOADS: usize = 4;
/// The most relative paths of one data source tried, in their order.
const MAX_PATHS: usize = 16;
/// Open statuses meaning the host does not have the file: 0x4fb5,
/// FileMaker's `kFileNotFound`, which FileMaker Server answers for a file it
/// does not host (network-protocol.md 5.5) and OpenMaker Server answers
/// since #914 of github.com/OpenMaker-Dev/openmaker, for a closed hosted
/// file too since #939 there, as FileMaker Server answers it; and 4, which
/// OpenMaker Server answers for a hosted file that is closing or opening,
/// and answered for a missing name before #914 and for a closed file before
/// #939.
const NOT_HOSTED: [u16; 2] = [4, 0x4fb5];

/// Keep a bounded alert while counting problems omitted from it.
#[derive(Default)]
struct Warnings {
    lines: Vec<String>,
    omitted: usize,
}

impl Warnings {
    /// Add `line` unless it is there already: the same problem reached
    /// through two data sources. Lines about different files differ, as
    /// each names its file.
    fn push(&mut self, line: String) {
        if self.lines.contains(&line) {
            return;
        }
        if self.lines.len() < MAX_WARNINGS - 1 {
            self.lines.push(line);
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }

    fn prepend(&mut self, line: String) {
        self.lines.insert(0, line);
        if self.lines.len() >= MAX_WARNINGS {
            self.lines.pop();
            self.omitted = self.omitted.saturating_add(1);
        }
    }

    fn finish(mut self) -> Vec<String> {
        if self.omitted > 0 {
            self.lines.push(format!(
                "…and {} more problems with related files.",
                self.omitted
            ));
        }
        self.lines
    }
}

impl Extend<String> for Warnings {
    fn extend<T: IntoIterator<Item = String>>(&mut self, lines: T) {
        for line in lines {
            self.push(line);
        }
    }
}

impl FromIterator<String> for Warnings {
    fn from_iter<T: IntoIterator<Item = String>>(lines: T) -> Self {
        let mut warnings = Self::default();
        warnings.extend(lines);
        warnings
    }
}

/// The queue and its deduplication set retain at most [`MAX_SOURCES`]
/// data sources (`found` counts them; the queue also holds data sources
/// waiting to try their next path). Opens are limited apart from this
/// ([`MAX_FILES`]), so a data source naming a file already open, which
/// needs none, is never left out by it.
fn discover(
    bindings: &[Binding],
    pending: &mut Vec<(Target, usize)>,
    seen: &mut BTreeSet<OsString>,
    found: &mut usize,
    limited: &mut bool,
) {
    for binding in bindings {
        if seen.contains(&binding.target.key) {
            continue;
        }
        if *found == MAX_SOURCES {
            *limited = true;
            break;
        }
        *found += 1;
        seen.insert(binding.target.key.clone());
        pending.push((binding.target.clone(), 0));
    }
}

/// The opened file's credentials, for its related files on the same host.
struct Auth {
    host: String,
    port: u16,
    account: String,
    password: Zeroizing<String>,
    /// A handle on the opened file's connection, while its related files
    /// open: each one on that host opens on a handle of it
    /// ([`Session::share`]), as FileMaker Pro opens related files in the
    /// session it has (network-protocol.md 5.5). `None` opens each on a
    /// connection of its own.
    connection: Option<Session>,
    /// The opened file's account, offered to another host or port once
    /// ([`Credentials::Offer`]): kept for that host only once a file there
    /// admits it.
    offered: bool,
}

/// A file a data source's path names on a host: the host and port it is
/// on (a relative path's are those of the file whose data source names it,
/// an `fmnet` path's its own), and its name there. On this computer the
/// host is empty and the port 0: a file there is told apart by its path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Located {
    host: String,
    port: u16,
    name: String,
}

impl Located {
    /// What tells the file apart: its host's text in any case, its port,
    /// and its name as FileMaker compares names ([`hosted_key`]). The same
    /// name on two hosts or ports is two files.
    fn key(&self) -> OsString {
        format!(
            "{}:{}/{}",
            self.host.to_lowercase(),
            self.port,
            hosted_key(&self.name)
        )
        .into()
    }

    /// Whether it is on `host` at `port`, the host's text in any case.
    fn on(&self, (host, port): (&str, u16)) -> bool {
        self.port == port && self.host.eq_ignore_ascii_case(host)
    }

    /// The file as an `fmnet` path: how the window asks to sign in to a
    /// file on another host than the opened file's, and what
    /// [`ExternalFiles::sign_in`] reads back.
    fn fmnet(&self) -> String {
        format!("fmnet:/{}:{}/{}", self.host, self.port, self.name)
    }
}

/// A related file a data source names: its name as referenced, what tells
/// it apart (its host, port and the name the host knows it by,
/// [`Located::key`], or for a file on this computer its path, links
/// followed) and, on this computer, the file's path when one is there.
#[derive(Clone, Debug)]
struct Target {
    name: String,
    key: OsString,
    path: Option<PathBuf>,
    /// On a host: the data source's paths, each on the host and port it
    /// names, in their order, which [`ExternalFiles::hydrate`] tries until
    /// one names a file its host has. Its key is theirs ([`paths_key`]).
    paths: Vec<Located>,
    /// The file whose data source names it (the first found), and that
    /// data source's name when it can be read: what a warning says.
    from: String,
    source: Option<String>,
}

/// An occurrence showing another file's table: the occurrence's id in the
/// file that has it, the referenced file and the table's id there.
#[derive(Clone, Debug)]
struct Binding {
    occurrence: u64,
    /// The occurrence's name, for a warning.
    occurrence_name: String,
    target: Target,
    table: u64,
}

/// Where the related files of a file opened from this computer are opened:
/// the family's hosts (local.rs).
struct LocalFiles {
    hosts: Arc<LocalHosts>,
}

struct RelatedFile {
    /// Its name without the extension, as first referenced (on this
    /// computer: the file's own name).
    name: String,
    /// What tells it apart ([`Target::key`]).
    key: OsString,
    /// Its path, for a file on this computer.
    path: Option<PathBuf>,
    /// For a hosted file, its `Get ( FilePath )` naming its own host
    /// ([`ExternalFiles::name_hosted`]); `None` on this computer.
    hosted_path: Option<String>,
    /// Which file it is: its host, port and name, or its path on this
    /// computer.
    identity: om_data::file_refs::FileIdentity,
    /// The same record/rule engine as the root, bound to this owning file.
    owner: RefCell<crate::NativeProvider>,
    /// Its own calculation schema, in its own ids.
    schema: CalcSchema,
    tables: Vec<om_hbam::Table>,
    bindings: Vec<Binding>,
    /// Its data sources that name no file this client opens.
    warnings: Warnings,
}

impl RelatedFile {
    /// Read the field catalogs `[T, 3]` of `tables` that this file's schema
    /// still lists without fields ([`CalcSchema::are_fields_pending`]), on
    /// its own session, and build its schema again with them: the same ids,
    /// since a table's fields keep their own and the tables, occurrences,
    /// functions and value lists come from catalogs read at open. One its
    /// owner read already (for a write) is not read again.
    fn read_tables(
        &mut self,
        tables: &BTreeSet<u64>,
        stop: &dyn Fn() -> bool,
    ) -> Result<(), DataError> {
        let wanted: Vec<u64> = tables
            .iter()
            .copied()
            .filter(|&table| self.schema.are_fields_pending(table))
            .collect();
        // Hydration revisits already-open files too. Their loaded catalogs
        // need no decoding or owner-schema rebuild. A pending catalog still
        // rebuilds below even if the owner downloaded it for a write already.
        if wanted.is_empty() {
            return Ok(());
        }
        let owner = self.owner.get_mut();
        read_table_catalogs(owner, &wanted, stop)?;
        self.schema = rebuild_owner_schema(owner, &self.schema)?;
        self.tables = owner.catalog.tables.clone();
        Ok(())
    }
}

/// The path of table `table`'s field catalog `[T, 3]`.
fn field_catalog(table: u64) -> Vec<Vec<u8>> {
    vec![key_bytes(table), key_bytes(3)]
}

/// Read into `owner`'s downloads the field catalogs `[T, 3]` of `tables`
/// they lack, on its session. `true` when any was read.
fn read_table_catalogs(
    owner: &mut crate::NativeProvider,
    tables: &[u64],
    stop: &dyn Fn() -> bool,
) -> Result<bool, DataError> {
    let mut read = false;
    for &table in tables {
        if stop() {
            break;
        }
        if owner
            .schema_downloads
            .iter()
            .any(|d| d.path == field_catalog(table))
        {
            continue;
        }
        let session = owner.session_mut()?;
        let fields = download(session, &[table, 3])?;
        owner.schema_downloads.push(fields);
        read = true;
    }
    if read {
        // A key tree is built in key order.
        owner.schema_downloads.sort_by(|a, b| a.path.cmp(&b.path));
    }
    Ok(read)
}

/// `owner`'s catalog and calculation schema built again from its downloads,
/// as its file's open built them, with the account and privileges `previous`
/// has: a table whose field catalog it has not read is marked as such
/// ([`CalcSchema::set_fields_pending`]). Its own schema for owner plans is
/// dropped. The schema, for the file's copy.
fn rebuild_owner_schema(
    owner: &mut crate::NativeProvider,
    previous: &CalcSchema,
) -> Result<CalcSchema, DataError> {
    let tree = download_tree(&owner.schema_downloads)?;
    let catalog = Catalog::from_tree(&tree).map_err(|e| failure("decode the file's fields", e))?;
    let mut schema = crate::calc::calc_schema(&tree, &catalog, &previous.file_name);
    schema.authenticated_account = previous.authenticated_account.clone();
    schema.withhold_hidden_layouts();
    schema.authenticated_privileges = previous.authenticated_privileges.clone();
    for table in &catalog.tables {
        if !owner
            .schema_downloads
            .iter()
            .any(|d| d.path == field_catalog(table.id))
        {
            schema.set_fields_pending(table.id);
        }
    }
    owner.catalog = catalog;
    owner.calc.schema = Some(Arc::new(schema.clone()));
    *owner.own_schema.get_mut() = None;
    Ok(schema)
}

/// Where an imported table's data is read: a related file's connection,
/// under the table's id in that file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TableRoute {
    file: usize,
    pub table: u64,
}

impl TableRoute {
    /// This opened root's index for the owning file (not a host file id).
    pub(crate) fn file_index(self) -> usize {
        self.file
    }

    /// Whether `other` routes to the same related file.
    pub(crate) fn same_file(self, other: TableRoute) -> bool {
        self.file == other.file
    }
}

/// What an owning file's records are planned over when the opened file
/// writes them (#187, related_owner.rs): the owning file's own schema with
/// the opened file's imported and the owner's occurrences of it bound
/// ([`ExternalFiles::owner_plan`]).
pub(crate) struct OwnerPlan {
    pub schema: std::sync::Arc<CalcSchema>,
    /// The opened file's tables as the owner's plan names them (client-only
    /// ids there), to their ids in the opened file.
    pub tables: BTreeMap<u64, u64>,
    /// The owner's other related files the opened file has open, imported
    /// into the plan as the opened file is: read through their own
    /// sessions, which the opened file lends for the call
    /// ([`ExternalFiles::lend_session`], #1160).
    pub others: Vec<OtherPlan>,
}

/// One of [`OwnerPlan::others`]: the related file's index here, and its
/// tables as the owner's plan names them, to their ids in that file.
pub(crate) struct OtherPlan {
    pub file: usize,
    pub tables: BTreeMap<u64, u64>,
}

// Read topology carries no record, permission or acquisition authority.
// Preserve the accepted imports of passive files as well as asset owners.
#[derive(Clone)]
pub(crate) struct RecoveryTopology {
    on_demand: bool,
    demands: Vec<Vec<u64>>,
    files: Vec<TopologyFile>,
}

#[derive(Clone)]
struct TopologyFile {
    identity: om_data::file_refs::FileIdentity,
    creation: u32,
    ids: ImportedFileMap,
}

impl RecoveryTopology {
    pub(crate) fn demands(&self) -> &[Vec<u64>] {
        &self.demands
    }

    pub(crate) fn matches(&self, current: &ExternalFiles) -> bool {
        self.on_demand == current.on_demand
            && current.recovery_topology_available()
            && self.files.len() == current.files.len()
            && self
                .files
                .iter()
                .zip(current.files.iter().zip(&current.import_maps))
                .all(|(old, (file, ids))| {
                    old.identity == file.identity
                        && file
                            .owner
                            .try_borrow()
                            .is_ok_and(|owner| owner.recovery_creation == Some(old.creation))
                        && old.ids.tables == ids.tables
                        && old.ids.occurrences == ids.occurrences
                        && old.ids.functions == ids.functions
                        && old.ids.value_lists == ids.value_lists
                        && old.ids.relationships == ids.relationships
                })
    }
}

/// The related files of the open file.
#[derive(Default)]
pub(crate) struct ExternalFiles {
    auth: Option<Auth>,
    /// What tells the opened file apart ([`Target::key`]).
    root: OsString,
    /// Which file the opened file is, as an owner's import of it names it.
    root_identity: Option<om_data::file_refs::FileIdentity>,
    /// Each data source an import resolved, by its key, to the file its
    /// search of the source's paths found ([`Self::next_path`]): the opened
    /// file itself (`None`; also a list of paths whose path that opened is
    /// among them) or a related file, by its index. A source whose search
    /// stopped short (its file refused the account, was not reached or was
    /// not tried yet) or found no file is not here. What an owner's
    /// occurrences are bound by ([`Self::owner_plan`]). Kept until the
    /// file closes.
    resolved: BTreeMap<OsString, Option<usize>>,
    /// Each owning file's [`OwnerPlan`], by its index, built when first
    /// asked and dropped whenever the files are imported again or closed.
    owner_plans: RefCell<BTreeMap<usize, std::sync::Arc<OwnerPlan>>>,
    /// The Tickle interval of each related connection (the open file's).
    keepalive: Option<Duration>,
    /// Where the opened file's additional certificate authority is trusted
    /// ([`crate::NativeProvider::with_ca_scope`]).
    ca_scope: Option<(String, u16)>,
    files: Vec<RelatedFile>,
    /// What went wrong opening each file that could not be, by
    /// [`Target::key`]: it is not tried again until the file is reopened.
    failed: BTreeMap<OsString, String>,
    /// The files the host answered it does not have, by key: a data source
    /// listing one goes on to its next path, and none is tried again until
    /// the file is reopened.
    not_hosted: BTreeSet<OsString>,
    /// Imported tables by client-only id.
    routes: BTreeMap<u64, TableRoute>,
    warnings: Vec<String>,
    /// The opened file is one on this computer ([`Self::for_local_file`]):
    /// its related files are beside it.
    local: Option<LocalFiles>,
    /// The files that refused the session's credentials (refusals.rs).
    refusals: crate::refusals::Shared,
    /// The host and port a hosted file was opened from: its relative paths'
    /// host, and the one whose files are sent its account; `None` for a
    /// file on this computer, which follows no `fmnet` path. Kept after the
    /// credentials go, so a schema built again binds the same files.
    endpoint: Option<(String, u16)>,
    /// The accounts other hosts or ports than the opened file's admitted
    /// (#976): typed for one of their files, or the opened file's offered
    /// there ([`Credentials::Offer`]), one per host and port. Kept until
    /// the file closes or reconnects, since a script or a later read may
    /// open another file there: each goes only to its own host and port.
    other_hosts: Vec<Auth>,
    /// The files on another host or port in [`Self::refused`] that wait for
    /// an account of that host's, by key: no Login of theirs was refused.
    /// An account signed in to there lets the next schema try them all.
    awaiting: BTreeSet<OsString>,
    /// Closed ([`Self::close`]): the file is closing, or a Close stopped
    /// its related files opening. No account is lent to it again
    /// ([`Self::lend`]); opening the file again starts a new one.
    closed: bool,
    /// The related files whose Login refused the session's credentials, in
    /// the order found, each with the data sources whose search ended at
    /// it: each may be signed in to with another account
    /// ([`Self::sign_in`], #214).
    refused: Vec<(Target, BTreeSet<OsString>)>,
    /// The opened file's occurrences of each data source, by the data
    /// source's key: what of the file a refused file's tables would show.
    occurrences: BTreeMap<OsString, BTreeSet<u64>>,
    /// Open a hosted file's related files only when something reads them
    /// ([`Self::set_on_demand`], #983): a data source no read has asked
    /// for ([`Self::want`]) is not tried, and its occurrences wait
    /// ([`CalcSchema::set_pending`]), with no warning.
    on_demand: bool,
    /// The related files whose host could not be reached (#740): the
    /// connection to it failed (host lookup, connect or TLS), before any
    /// Open or Login. By key, when that was and what it said. Not failed for the session:
    /// one is tried again when the schema is built next, or on demand when
    /// a read wants it once [`Self::retry_after`] has passed.
    unreachable: BTreeMap<OsString, (std::time::Instant, String)>,
    /// How long a file whose host could not be reached waits before it is
    /// tried again ([`Self::set_retry_after`]; [`RETRY_AFTER`] unless set).
    retry_after: Option<Duration>,
    /// The data sources to open, by [`Target::key`].
    wanted: BTreeSet<OsString>,
    /// Of each file to open or open, by [`Target::key`], the tables whose
    /// field catalogs `[T, 3]` a read asked for: on demand, a file's open
    /// reads only those, and the next [`Self::hydrate`] reads those asked
    /// for since (lazy field catalogs, #772). In the file's own ids.
    wanted_tables: BTreeMap<OsString, BTreeSet<u64>>,
    /// The opened file's occurrences of other files' tables that something
    /// read: the warnings of their data sources naming no file this client
    /// opens are given once one is read.
    read: BTreeSet<u64>,
    /// The opened file's occurrences of other files' tables, as of the last
    /// [`Self::hydrate`].
    root_bindings: Vec<Binding>,
    /// Local variable-backed occurrences wait for their first read. Once
    /// a source opens successfully, its binding stays for the session.
    variable_occurrences: BTreeSet<u64>,
    variable_targets: BTreeMap<u64, Target>,
    file_variables: std::sync::Arc<om_data::calc::eval::Variables>,
    /// Each imported occurrence (client-only id): its file's index in
    /// [`Self::files`] and its id there, as of the last [`Self::hydrate`].
    imported: BTreeMap<u64, (usize, u64)>,
    /// Published mappings survive catalog growth, including missing references.
    import_maps: Vec<ImportedFileMap>,
    // Read demand groups, including passive reads, in their original order.
    // Bound the total recorded occurrences; incomplete history refuses recovery.
    recovery_demands: Vec<Vec<u64>>,
    recovery_demand_count: usize,
    recovery_topology_incomplete: bool,
}

impl ExternalFiles {
    pub(crate) fn recovery_topology_available(&self) -> bool {
        !self.closed
            && !self.recovery_topology_incomplete
            && self.files.len() == self.import_maps.len()
            && self.files.iter().all(|file| {
                file.owner
                    .try_borrow()
                    .is_ok_and(|owner| owner.recovery_creation.is_some())
            })
    }

    pub(crate) fn recovery_topology(&self) -> Option<RecoveryTopology> {
        if !self.recovery_topology_available() {
            return None;
        }
        let files = self
            .files
            .iter()
            .zip(&self.import_maps)
            .map(|(file, ids)| {
                Some(TopologyFile {
                    identity: file.identity.clone(),
                    creation: file.owner.try_borrow().ok()?.recovery_creation?,
                    ids: ids.clone(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(RecoveryTopology {
            on_demand: self.on_demand,
            demands: self.recovery_demands.clone(),
            files,
        })
    }

    /// Ready to open the related files of the file `connection` opened.
    pub fn for_connection(connection: &Connection, keepalive: Option<Duration>) -> Self {
        let mut files = Self::default();
        files.auth = Some(Auth {
            host: connection.host.clone(),
            port: connection.port,
            account: connection.account.clone(),
            password: Zeroizing::new(connection.password.clone()),
            connection: None,
            offered: false,
        });
        files.root = Located {
            host: connection.host.clone(),
            port: connection.port,
            name: connection.file.clone(),
        }
        .key();
        files.root_identity = Some(om_data::file_refs::FileIdentity::hosted(
            &connection.host,
            connection.port,
            &connection.file,
        ));
        files.keepalive = keepalive;
        files.endpoint = Some((connection.host.clone(), connection.port));
        files
    }

    /// Sharing the session's memory of refused Logins: a file that refused
    /// these credentials is not sent them again (refusals.rs).
    pub fn remembering(mut self, refusals: crate::refusals::Shared) -> Self {
        self.refusals = refusals;
        self
    }

    /// Ready to open the related files of the file at `path` (links
    /// followed) that `connection` opened from this computer: beside it,
    /// through the family's `hosts`, with `connection`'s account (module
    /// docs).
    pub fn for_local_file(
        path: &Path,
        connection: &Connection,
        keepalive: Option<Duration>,
        hosts: Arc<LocalHosts>,
    ) -> Self {
        let mut files = Self::for_connection(connection, keepalive);
        files.root = path.as_os_str().to_owned();
        files.root_identity = Some(om_data::file_refs::FileIdentity::local(path));
        files.local = Some(LocalFiles { hosts });
        files.endpoint = None;
        files
    }

    /// The folder the opened file's related files are in, for a file on
    /// this computer.
    fn local_folder(&self) -> Option<PathBuf> {
        self.local.as_ref()?;
        Path::new(&self.root).parent().map(Path::to_path_buf)
    }

    pub(crate) fn set_file_variables(&mut self, variables: &om_data::calc::eval::Variables) {
        if self.local.is_some() && self.file_variables.revision() != variables.revision() {
            self.file_variables = std::sync::Arc::new(variables.clone());
            self.read
                .retain(|occurrence| !self.variable_occurrences.contains(occurrence));
        }
    }

    pub(crate) fn file_variables(&self) -> om_data::rules::WriteFileVariables {
        om_data::rules::WriteFileVariables::new(std::sync::Arc::clone(&self.file_variables))
    }

    pub(crate) fn local_source_variable(&self, name: &str) -> Option<&str> {
        use om_data::calc::Value;
        let Value::Text(value) = self.file_variables.get(name, 1)? else {
            return None;
        };
        (!value.as_str().is_empty()).then(|| value.as_str())
    }

    /// Whether a related file named `name` (with or without `.fmp12`, any
    /// case) is open beside a file opened from this computer. A family has
    /// one file of a name open (local.rs).
    pub fn has_local_file(&self, name: &str) -> bool {
        if !om_data::file_refs::is_plain_name(name) {
            let path = self
                .local_folder()
                .and_then(|folder| om_data::file_refs::local_database_path(name, &folder))
                .and_then(|path| path.canonicalize().ok());
            return path.is_some() && self.files.iter().any(|file| file.path == path);
        }
        let key = hosted_key(name);
        self.files.iter().any(|file| {
            file.path
                .as_deref()
                .and_then(Path::file_name)
                .is_some_and(|n| hosted_key(&n.to_string_lossy()) == key)
        })
    }

    /// Another lease of each related file's host on this computer: held
    /// across a reconnect, so the files stay open in the same hosts.
    pub fn local_leases(&self) -> Vec<Lease> {
        self.files
            .iter()
            .filter_map(|file| file.owner.try_borrow().ok()?.local.clone())
            .collect()
    }

    /// Lend the next [`Self::hydrate`] the account the opened file is open
    /// with, which the provider keeps for its scripts' opens, once this
    /// copy of it has gone: a related file first reached when the schema
    /// is built again (a data source's next path after another host turned
    /// out not to have its file, or one first found then) opens with it
    /// (#978). As the copy it replaces, it goes only to files on the
    /// opened file's own host and port, or on this computer
    /// ([`Self::credentials_for`]), and is dropped when the schema is
    /// built.
    pub fn lend(&mut self, account: &str, password: &str) {
        if self.auth.is_some() || self.closed {
            return;
        }
        // A file on this computer opens its files through their own hosts:
        // its host and port are not used.
        let (host, port) = self.endpoint.clone().unwrap_or_default();
        self.auth = Some(Auth {
            host,
            port,
            account: account.to_owned(),
            password: Zeroizing::new(password.to_owned()),
            connection: None,
            offered: false,
        });
    }

    /// The account a file on `host` at `port`, another host or port than
    /// the opened file's, admitted in this session: one typed for it, or
    /// the opened file's offered there ([`Self::other_hosts`]). A script's
    /// open of a file there is sent it first (`NativeProvider::open_peer_with`).
    pub fn accepted_on(&self, host: &str, port: u16) -> Option<(&str, &str)> {
        self.other_hosts
            .iter()
            .find(|auth| !auth.offered && auth.port == port && auth.host.eq_ignore_ascii_case(host))
            .map(|auth| (auth.account.as_str(), auth.password.as_str()))
    }

    /// `host` at `port`, another host or port than the opened file's,
    /// admitted `account` and `password` for a file a script opened there
    /// (`NativeProvider::open_peer_with`): that host's other files are sent
    /// them first, as those a sign-in there admits ([`Self::accepted_on`]).
    /// Kept until the file closes or reconnects, as the others.
    pub fn admitted_on(&mut self, host: &str, port: u16, account: &str, password: &str) {
        if self.closed {
            return;
        }
        self.other_hosts
            .retain(|other| !(other.port == port && other.host.eq_ignore_ascii_case(host)));
        self.other_hosts.push(Auth {
            host: host.to_owned(),
            port,
            account: account.to_owned(),
            password: Zeroizing::new(password.to_owned()),
            connection: None,
            offered: false,
        });
    }

    /// Open related files only when something reads them (#983), or every
    /// one when the schema is built (the default): a hosted file only; a
    /// file on this computer opens its related files when the schema is
    /// built.
    /// How long a related file whose host could not be reached waits before
    /// a read tries it again (#740): 30 seconds unless set.
    pub fn set_retry_after(&mut self, after: Option<Duration>) {
        self.retry_after = after;
    }

    /// How long until a related file whose host could not be reached is
    /// tried again, the soonest of them (#740); `None` when none waits.
    pub fn retry_in(&self) -> Option<Duration> {
        let after = self.retry_after.unwrap_or(RETRY_AFTER);
        self.unreachable
            .values()
            .map(|(when, _)| after.saturating_sub(when.elapsed()))
            .min()
    }

    pub fn set_on_demand(&mut self, on: bool) {
        self.on_demand = on && self.local.is_none();
    }

    /// A related file on another host or port than `scope` is not verified
    /// against the opened file's additional certificate authority.
    pub fn set_ca_scope(&mut self, scope: Option<(String, u16)>) {
        self.ca_scope = scope;
    }

    /// Whether any related files wait until something reads them.
    pub fn on_demand(&self) -> bool {
        self.on_demand || !self.variable_occurrences.is_empty()
    }

    /// A local variable source needs the root schema kept for its first read.
    pub fn has_local_variable_sources(&self, catalog: &Catalog, tree: &KeyTree<'_>) -> bool {
        let mut seen = BTreeSet::new();
        self.local.is_some()
            && catalog
                .table_occurrences
                .iter()
                .filter_map(|occurrence| occurrence.reference())
                .any(|(source, _)| {
                    source != 0
                        && seen.insert(source)
                        && matches!(source_files(tree, source, None), SourcePaths::Variable(_))
                })
    }

    /// Ask for the files `schema`'s occurrences `occurrences` show (the
    /// opened file's or imported ones'), on demand: those waiting for their
    /// files ([`CalcSchema::is_pending`]) are opened when the schema is
    /// built next, and the warnings of a data source naming no file this
    /// client opens are given once one of its occurrences is read. `true`
    /// when that build changes something, so the caller builds it.
    pub fn want(
        &mut self,
        schema: &CalcSchema,
        occurrences: impl IntoIterator<Item = u64>,
    ) -> bool {
        if !self.on_demand() {
            return false;
        }
        let mut more = false;
        let mut demand = Vec::new();
        let mut truncated = false;
        for occurrence in occurrences {
            if !self.on_demand && !self.variable_occurrences.contains(&occurrence) {
                continue;
            }
            if !self.recovery_topology_incomplete {
                if demand.len() < MAX_SOURCES {
                    demand.push(occurrence);
                } else {
                    truncated = true;
                }
            }
            // Bound to a table of an open file whose field catalog is not
            // read yet: that table is wanted, and read by the next hydrate.
            // Once this file's copy has it (its owner read it), the schema
            // is built again to take it.
            let unread = schema
                .table_of(occurrence)
                .filter(|table| schema.are_fields_pending(table.id))
                .and_then(|table| self.routes.get(&table.id).copied());
            if let Some(route) = unread {
                let Some(file) = self.files.get(route.file) else {
                    continue;
                };
                self.wanted_tables
                    .entry(file.key.clone())
                    .or_default()
                    .insert(route.table);
                // A previous hydrate may have failed. An unread table is
                // still demandable even when its request is already queued.
                more = true;
                continue;
            }
            if self.variable_occurrences.contains(&occurrence)
                && schema.external_occurrence(occurrence).is_some()
            {
                more |= self.read.insert(occurrence);
            } else if schema.is_pending(occurrence) {
                let binding = match self.imported.get(&occurrence) {
                    Some(&(file, own)) => self
                        .files
                        .get(file)
                        .and_then(|file| file.bindings.iter().find(|b| b.occurrence == own)),
                    None => self
                        .root_bindings
                        .iter()
                        .find(|b| b.occurrence == occurrence),
                };
                if let Some(binding) = binding {
                    // Its file's open reads this table's fields with it.
                    self.wanted_tables
                        .entry(binding.target.key.clone())
                        .or_default()
                        .insert(binding.table);
                    more |= self.wanted.insert(binding.target.key.clone());
                }
            } else if schema.external_occurrence(occurrence).is_some()
                && !self.imported.contains_key(&occurrence)
            {
                // The opened file's own, naming no file this client opens:
                // its warning is due.
                more |= self.read.insert(occurrence);
            }
        }
        if more && !self.recovery_topology_incomplete {
            if truncated || self.recovery_demand_count.saturating_add(demand.len()) > MAX_SOURCES {
                self.recovery_topology_incomplete = true;
            } else {
                self.recovery_demand_count += demand.len();
                self.recovery_demands.push(demand);
            }
        }
        more
    }

    // Demand belongs to a data source until its alternative paths select a
    // file. Keep it on the source too: a missing path may select another file.
    fn carry_table_demand(&mut self, source: &OsString, file: &OsString) {
        if source != file {
            if let Some(tables) = self.wanted_tables.get(source).cloned() {
                self.wanted_tables
                    .entry(file.clone())
                    .or_default()
                    .extend(tables);
            }
        }
    }

    /// Drop the opened file's host's account a sign-in lent the schema
    /// build ([`Self::sign_in`]), once built: the accounts other hosts
    /// admitted stay until the file closes or reconnects.
    pub fn drop_lent_credentials(&mut self) {
        self.auth = None;
    }

    /// The schema the owning file of `route` plans its records over when
    /// the opened file writes them (#187): its own, with `root_own` (the
    /// opened file's own schema, read only when not cached) imported as the
    /// opened file, and each of its occurrences whose data source found the
    /// opened file bound to it: a binding whose data source an import
    /// resolved to the opened file (`resolved`), the rule
    /// [`Self::import`] binds by. Its occurrences of another related file
    /// the opened file has open are imported and bound the same way
    /// (#1160), to the file their data source resolved to, never to one a
    /// later path names past a path whose search stopped there. A source
    /// not resolved to an open file stays unresolved. Held participants
    /// remain in the schema; the coordinator supplies their snapshots or
    /// marks their provisional values unknown (#1198).
    pub(crate) fn owner_plan(
        &self,
        route: TableRoute,
        root_own: impl FnOnce() -> Result<std::sync::Arc<CalcSchema>, DataError>,
        root_path: Option<String>,
    ) -> Result<std::sync::Arc<OwnerPlan>, DataError> {
        if let Some(plan) = self.owner_plans.borrow().get(&route.file) {
            return Ok(std::sync::Arc::clone(plan));
        }
        let file = self
            .files
            .get(route.file)
            .ok_or_else(|| DataError::new("The related file is not open."))?;
        let root_own = root_own()?;
        let mut schema = file.schema.clone();
        let map = schema.import_owned_file_schema(&root_own, root_path, self.root_identity.clone());
        let resolved = |target: &Target| match target.key == self.root {
            true => Some(None),
            false => self.resolved.get(&target.key).copied(),
        };
        for binding in file
            .bindings
            .iter()
            .filter(|b| resolved(&b.target) == Some(None))
        {
            if let Some(&table) = map.tables.get(&binding.table) {
                schema.bind_external_occurrence(binding.occurrence, table);
            }
        }
        let mut others = Vec::new();
        for (index, other) in self.files.iter().enumerate() {
            if index == route.file {
                continue;
            }
            let named: Vec<&Binding> = file
                .bindings
                .iter()
                .filter(|b| resolved(&b.target) == Some(Some(index)))
                .collect();
            if named.is_empty() {
                continue;
            }
            let path = other
                .path
                .as_deref()
                .map(om_data::file_refs::local_file_path)
                .or_else(|| other.hosted_path.clone());
            let imported =
                schema.import_owned_file_schema(&other.schema, path, Some(other.identity.clone()));
            for binding in named {
                if let Some(&table) = imported.tables.get(&binding.table) {
                    schema.bind_external_occurrence(binding.occurrence, table);
                }
            }
            others.push(OtherPlan {
                file: index,
                tables: imported
                    .tables
                    .iter()
                    .map(|(&own, &client)| (client, own))
                    .collect(),
            });
        }
        let plan = std::sync::Arc::new(OwnerPlan {
            schema: std::sync::Arc::new(schema),
            tables: map
                .tables
                .iter()
                .map(|(&own, &client)| (client, own))
                .collect(),
            others,
        });
        self.owner_plans
            .borrow_mut()
            .insert(route.file, std::sync::Arc::clone(&plan));
        Ok(plan)
    }

    /// The related file client-only table `table` routes to, by its index
    /// here.
    pub(crate) fn file_of_table(&self, table: u64) -> Option<usize> {
        self.routes.get(&table).map(|route| route.file)
    }

    /// The opened file's client-only id for table `own` of `route`'s file:
    /// the table that routes there ([`Self::route`]'s inverse).
    pub(crate) fn client_table(&self, route: TableRoute, own: u64) -> Option<u64> {
        self.routes
            .iter()
            .find(|(_, at)| at.file == route.file && at.table == own)
            .map(|(&client, _)| client)
    }

    /// The opened file's occurrence for the owning file of `route`'s own
    /// occurrence `own`, as that file's import named it: how the opened
    /// file asks for the files an owning file's rules read (#1160).
    pub(crate) fn imported_occurrence(&self, route: TableRoute, own: u64) -> Option<u64> {
        self.imported
            .iter()
            .find(|(_, &(file, o))| file == route.file && o == own)
            .map(|(&client, _)| client)
    }

    /// Related file `file`'s session and its own schema, lent to an owning
    /// file's routed write for the call ([`OwnerPlan::others`]); given back
    /// with [`Self::return_session`]. `None` when it has none, or is in use.
    pub(crate) fn lend_session(
        &self,
        file: usize,
    ) -> Option<(Session, std::sync::Arc<CalcSchema>)> {
        let mut owner = self.files.get(file)?.owner.try_borrow_mut().ok()?;
        let schema = owner.calc.schema.clone()?;
        Some((owner.session.take()?, schema))
    }

    /// Give related file `file` back the session [`Self::lend_session`]
    /// lent.
    pub(crate) fn return_session(&self, file: usize, session: Session) {
        if let Some(Ok(mut owner)) = self.files.get(file).map(|f| f.owner.try_borrow_mut()) {
            owner.session = Some(session);
        }
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.wanted_tables.clear();
        self.import_maps.clear();
        self.recovery_demands.clear();
        self.recovery_demand_count = 0;
        self.recovery_topology_incomplete = false;
        self.unreachable.clear();
        self.auth = None;
        self.other_hosts.clear();
        self.awaiting.clear();
        self.routes.clear();
        self.owner_plans.get_mut().clear();
        self.resolved.clear();
        self.variable_targets.clear();
        self.variable_occurrences.clear();
        self.file_variables = Default::default();
        self.failed.clear();
        self.not_hosted.clear();
        self.refused.clear();
        self.occurrences.clear();
        self.warnings.clear();
        // Each owning provider disconnects and releases its locks on drop.
        self.files.clear();
    }

    fn note_refused(&mut self, file: &Target, source: &OsString) {
        match self.refused.iter_mut().find(|(t, _)| t.key == file.key) {
            Some((_, sources)) => {
                sources.insert(source.clone());
            }
            None => self
                .refused
                .push((file.clone(), BTreeSet::from([source.clone()]))),
        }
    }

    /// The related files that refused the session's account and password,
    /// each with the opened file's occurrences of its tables: a layout
    /// using one needs the file, and the window asks for an account then
    /// (#214).
    pub fn sign_ins(&self) -> Vec<om_data::RelatedSignIn> {
        self.refused
            .iter()
            .map(|(target, sources)| om_data::RelatedSignIn {
                file: self.sign_in_name(target),
                occurrences: sources
                    .iter()
                    .filter_map(|source| self.occurrences.get(source))
                    .flatten()
                    .copied()
                    .collect(),
            })
            .collect()
    }

    /// Sign in to the related file `connection.file` names, one that
    /// refused the session's credentials, with `connection`'s account and
    /// password (#214): one Login, or none when its host refused that pair
    /// before in this session (refusals.rs). A refusal is noted as any
    /// other. The file opened is added after the others, so they keep
    /// their ids, and the next [`Self::hydrate`] opens the related files
    /// it references with the same account. A hosted file signs in on
    /// `root`, the opened file's connection, as the other related files
    /// opened (#740).
    pub fn sign_in(
        &mut self,
        config: &NativeConfig,
        cancel: &CloseRequests,
        root: Option<&Session>,
        connection: &Connection,
    ) -> Result<(), DataError> {
        let name = hosted_key(&connection.file);
        let index = self
            .refused
            .iter()
            .position(|(target, _)| {
                let shown = self.sign_in_name(target);
                shown == connection.file
                    || (self.on_opened_host(target) && hosted_key(&shown) == name)
            })
            .ok_or_else(|| {
                DataError::with_code(
                    100,
                    format!(
                        "“{}” is not a related file that refused this file's account.",
                        connection.file
                    ),
                )
            })?;
        let target = self.refused[index].0.clone();
        // On a host: the host and port the file is on, the opened file's
        // connection only when it is that host's.
        let (host, port) = match (&self.local, target.paths.first()) {
            (None, Some(located)) => (located.host.clone(), located.port),
            _ => (connection.host.clone(), connection.port),
        };
        let shared = self.local.is_none() && self.on_opened_host(&target);
        let mut auth = Auth {
            host,
            port,
            account: connection.account.clone(),
            password: Zeroizing::new(connection.password.clone()),
            connection: root.filter(|_| shared).map(Session::share),
            offered: false,
        };
        let earlier = self.refusals.lock().expect("login refusals").refused(
            &refusal_key(&auth, &target),
            &auth.account,
            &auth.password,
        );
        if let Some(error) = earlier {
            return Err(error);
        }
        let config = crate::scoped_config(config, self.ca_scope.as_ref(), &auth.host, auth.port);
        let loaded = load(
            &config,
            &auth,
            self.local.as_ref(),
            self.keepalive,
            cancel,
            &self.refusals,
            (&target, None),
        );
        let file = match loaded {
            Ok(file) => file,
            // Another host that does not have the file: as when the schema
            // is built, the data sources waiting at it go on to their next
            // path when it is built again. No account was checked, so none
            // is kept.
            Err(error) if self.local.is_none() && is_not_hosted(&error) => {
                self.not_hosted.insert(target.key.clone());
                self.release(index);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        // Neither it nor the data sources that ended at it stay failed: the
        // next schema finds it open.
        self.release(index);
        let mut file = file;
        self.name_hosted(&mut file, &target);
        self.files.push(file);
        // The credentials wait for the next schema; the handle does not keep
        // the connection meanwhile. Another host's go with that host only.
        auth.connection = None;
        if self.local.is_none() && !self.on_opened_host(&target) {
            // That host's other files waiting for an account are tried with
            // this one when the schema is built next.
            let waiting: Vec<usize> = self
                .refused
                .iter()
                .enumerate()
                .filter(|(_, (other, _))| {
                    self.awaiting.contains(&other.key)
                        && other
                            .paths
                            .first()
                            .is_some_and(|located| located.on((&auth.host, auth.port)))
                })
                .map(|(at, _)| at)
                .collect();
            for at in waiting.into_iter().rev() {
                self.release(at);
            }
            self.other_hosts.retain(|other| {
                !(other.port == auth.port && other.host.eq_ignore_ascii_case(&auth.host))
            });
            self.other_hosts.push(auth);
        } else {
            self.auth = Some(auth);
        }
        Ok(())
    }

    /// Take entry `index` off [`Self::refused`]: neither the file nor the
    /// data sources whose search ended at it stay failed, so the next
    /// schema takes them up again.
    fn release(&mut self, index: usize) {
        let (target, sources) = self.refused.remove(index);
        self.awaiting.remove(&target.key);
        self.failed.remove(&target.key);
        for source in &sources {
            self.failed.remove(source);
        }
    }

    /// Whether related file `target` is on the host and port the file was
    /// opened from (always, on this computer).
    /// Name hosted `file`, opened as `target` (its one path), by its own
    /// host when that is not the opened file's ([`hosted_path`]): its
    /// `Get ( FilePath )`, in its formulas and in its owning provider's. A
    /// file on the opened file's host and port keeps the path derived beside
    /// the opened file's, as before; a file on this computer keeps its own.
    fn name_hosted(&self, file: &mut RelatedFile, target: &Target) {
        let (Some(located), None) = (target.paths.first(), &file.path) else {
            return;
        };
        if self.local.is_some() {
            return;
        }
        let opened = self
            .endpoint
            .as_ref()
            .map(|(host, port)| (host.as_str(), *port));
        if let Some(path) = hosted_path(opened, located, &file.name) {
            file.owner.get_mut().calc.file_path = Some(path.clone());
            file.hosted_path = Some(path);
        }
    }

    fn on_opened_host(&self, target: &Target) -> bool {
        match (&self.endpoint, target.paths.first()) {
            (Some((host, port)), Some(located)) => located.on((host, *port)),
            _ => true,
        }
    }

    /// How the window names related file `target` when it asks for an
    /// account: its name on the opened file's host, else its `fmnet` path,
    /// so that two files of one name on two hosts are asked for apart.
    fn sign_in_name(&self, target: &Target) -> String {
        match target.paths.first() {
            Some(located) if !self.on_opened_host(target) => located.fmnet(),
            _ => target.name.clone(),
        }
    }

    /// Whether the credentials are still held.
    #[cfg(test)]
    fn holds_credentials(&self) -> bool {
        self.auth.is_some()
    }

    /// The related file and table id of imported table `table`; `None` for
    /// a table of the opened file.
    pub(crate) fn route_tables(&self, route: TableRoute) -> Vec<(u64, u64)> {
        self.routes
            .iter()
            .filter(|(_, r)| r.same_file(route))
            .map(|(&client, r)| (client, r.table))
            .collect()
    }

    pub(crate) fn root_identity(&self) -> Option<&om_data::file_refs::FileIdentity> {
        self.root_identity.as_ref()
    }

    pub(crate) fn route_identity(
        &self,
        route: TableRoute,
    ) -> Option<om_data::file_refs::FileIdentity> {
        self.files.get(route.file).map(|file| file.identity.clone())
    }

    pub fn route(&self, table: u64) -> Option<TableRoute> {
        self.routes.get(&table).copied()
    }

    /// The owning file's field definitions, using the imported table id the
    /// client schema uses. Wire requests still go through `route`.
    /// `None` for a table whose field catalog is not read yet: its
    /// occurrences wait for it ([`CalcSchema::are_fields_pending`]), and it
    /// is never handed out as a table with no fields.
    pub fn table(&self, table: u64) -> Option<om_hbam::Table> {
        let route = self.route(table)?;
        let file = self.files.get(route.file)?;
        if file.schema.are_fields_pending(route.table) {
            return None;
        }
        let mut def = file.tables.iter().find(|t| t.id == route.table)?.clone();
        def.id = table;
        Some(def)
    }

    /// What the user should know about the related files: those that could
    /// not be opened, data sources that name no file this client opens,
    /// and tables the account cannot read. No credentials.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// A related file whose connection was lost, and why.
    pub fn connection_lost(&self) -> Option<(&str, String)> {
        self.files.iter().find_map(|file| {
            let owner = file.owner.try_borrow().ok()?;
            let reason = owner.session.as_ref()?.connection_lost()?;
            Some((file.name.as_str(), reason))
        })
    }

    /// Owning connections whose loss must expire their previously cached
    /// data. Keys distinguish separate sessions, including after rehydration.
    pub fn lost_connections(&self) -> std::collections::BTreeSet<[u8; 16]> {
        self.files
            .iter()
            .filter_map(|file| {
                let owner = file.owner.try_borrow().ok()?;
                let session = owner.session.as_ref()?;
                session
                    .connection_lost()
                    .is_some()
                    .then(|| *session.connection_key())
            })
            .collect()
    }

    /// One HostTimeStamp (op82) on each connection owning a related file
    /// other than `asked` (the opened file's, already asked), so that its
    /// reply brings the notifications waiting there; whether every one
    /// was answered (#804). Once `stop` holds (the window's read was
    /// superseded, #987) no further connection is asked: `None`. The
    /// connections asked keep what their replies brought; the pass is not
    /// complete, so the next read asks again.
    pub fn freshen_until(&self, asked: Option<[u8; 16]>, stop: &dyn Fn() -> bool) -> Option<bool> {
        let mut done: Vec<[u8; 16]> = asked.into_iter().collect();
        let mut answered = true;
        for file in &self.files {
            if stop() {
                return None;
            }
            let Ok(mut owner) = file.owner.try_borrow_mut() else {
                continue;
            };
            let Some(session) = &mut owner.session else {
                continue;
            };
            let key = *session.connection_key();
            if done.contains(&key) {
                continue;
            }
            done.push(key);
            answered &= session.host_timestamp().is_ok();
        }
        Some(answered)
    }

    /// Whether an owning connection delivered change notifications. Their
    /// wire table ids must not be interpreted as the root file's ids.
    pub fn take_notifications(&self) -> bool {
        let mut changed = false;
        for file in &self.files {
            if let Ok(mut owner) = file.owner.try_borrow_mut() {
                if let Some(session) = &mut owner.session {
                    changed |= !session.take_notifications().is_empty();
                }
            }
        }
        changed
    }

    /// `read` on the connection of the file `route` names, with the table's
    /// id in that file.
    pub fn with_session<T>(
        &self,
        route: TableRoute,
        read: impl FnOnce(&mut Session, u64) -> Result<T, DataError>,
    ) -> Result<T, DataError> {
        self.with_owner(route, |owner, table| read(owner.session_mut()?, table))
    }

    /// The font families of the file `route` reads, which its open does not
    /// read: its owner reads them once, the first time styled text of its
    /// records needs them (#1098).
    pub fn fonts(&self, route: TableRoute) -> Result<Arc<BTreeMap<u64, String>>, DataError> {
        self.with_owner(route, |owner, _| owner.file_fonts())
    }

    /// Execute with the owning file's complete write context. The caller's
    /// layout, scripts and imported ids stay in the root provider.
    pub fn with_owner<T>(
        &self,
        route: TableRoute,
        operation: impl FnOnce(&mut crate::NativeProvider, u64) -> Result<T, DataError>,
    ) -> Result<T, DataError> {
        let file = self
            .files
            .get(route.file)
            .ok_or_else(|| DataError::with_code(3, "The related file is not open."))?;
        let mut owner = file
            .owner
            .try_borrow_mut()
            .map_err(|_| DataError::new("The related file's connection is busy."))?;
        operation(&mut owner, route.table).map_err(|mut error| {
            error.message = format!("Related file “{}”: {}", file.name, error.message);
            error
        })
    }

    /// Open the related files `catalog` and `tree` (the opened file's)
    /// reference, as the module docs describe, and import their schemas
    /// into `schema`, the opened file's. Files already open are imported
    /// again in the same order, so a rebuilt schema gives their members the
    /// same ids. The credentials are dropped. Fails, with every related
    /// file closed, when a Close waits in `cancel` (the window is closing
    /// the file).
    pub fn hydrate(
        &mut self,
        config: &NativeConfig,
        cancel: &CloseRequests,
        root: Option<&Session>,
        schema: &mut CalcSchema,
        catalog: &Catalog,
        tree: &KeyTree<'_>,
    ) -> Result<(), DataError> {
        self.hydrate_until(config, cancel, root, schema, catalog, tree, &|| false)
    }

    /// [`Self::hydrate`], stopping before the next file it would open once
    /// `stop` holds (a window no longer needs the read that wanted them):
    /// the files opened so far are imported and kept, and the data sources
    /// still to open wait as if never asked for, neither failed nor warned
    /// about. A Close (`cancel`) still ends it with every file closed.
    #[allow(clippy::too_many_arguments)]
    pub fn hydrate_until(
        &mut self,
        config: &NativeConfig,
        cancel: &CloseRequests,
        root: Option<&Session>,
        schema: &mut CalcSchema,
        catalog: &Catalog,
        tree: &KeyTree<'_>,
        stop: &dyn Fn() -> bool,
    ) -> Result<(), DataError> {
        let mut auth = self.auth.take();
        let mut other_hosts = std::mem::take(&mut self.other_hosts);
        // Hosted related files open on the opened file's connection; on
        // this computer each file has its own host in the family.
        if let (Some(auth), Some(root), None) = (auth.as_mut(), root, &self.local) {
            auth.connection = Some(root.share());
        }
        // Another host's files open in the session a file open there has,
        // as FileMaker Pro opens a file's relative files in the session it
        // has on that host (network-protocol.md 5.5).
        for other in &mut other_hosts {
            other.connection = self.session_on(&other.host, other.port);
        }
        let folder = self.local_folder();
        let root_name = schema.file_name.clone();
        let endpoint = self
            .endpoint
            .as_ref()
            .map(|(host, port)| (host.as_str(), *port));
        let mut root_bindings = bindings(catalog, tree, folder.as_deref(), endpoint, &root_name);
        let mut variable_targets = BTreeMap::new();
        let mut variable_warnings = Warnings::default();
        self.variable_occurrences.clear();
        if let Some(folder) = &folder {
            let mut variables = BTreeMap::new();
            let occurrences: Vec<_> = catalog
                .table_occurrences
                .iter()
                .filter_map(|occurrence| {
                    let (source, table) = occurrence.reference().filter(|(id, _)| *id != 0)?;
                    let variable = variables.entry(source).or_insert_with(|| {
                        match source_files(tree, source, None) {
                            SourcePaths::Variable(name) => Some(name),
                            _ => None,
                        }
                    });
                    variable.is_some().then_some((occurrence, source, table))
                })
                .collect();
            let demanded: BTreeSet<_> = occurrences
                .iter()
                .filter_map(|(occurrence, source, _)| {
                    self.read.contains(&occurrence.id).then_some(*source)
                })
                .collect();
            for (occurrence, source, table) in occurrences {
                self.variable_occurrences.insert(occurrence.id);
                let target = if let Some(target) = self.variable_targets.get(&source) {
                    Some(target)
                } else if !demanded.contains(&source) {
                    schema.set_pending(occurrence.id);
                    None
                } else {
                    let variable = variables[&source].as_deref().expect("variable source");
                    variable_targets
                        .entry(source)
                        .or_insert_with(|| {
                            let path = self
                                .local_source_variable(variable)
                                .and_then(|value| {
                                    om_data::file_refs::local_database_path(value, folder)
                                })
                                .and_then(|path| path.canonicalize().ok())
                                .filter(|path| path.is_file());
                            let Some(path) = path else {
                                variable_warnings.push(format!(
                                    "The file “{variable}” could not be opened.  (Not Found)"
                                ));
                                return None;
                            };
                            let name = path.file_name()?.to_string_lossy().into_owned();
                            Some(Target {
                                key: path.as_os_str().to_owned(),
                                paths: vec![beside(&name)],
                                name,
                                path: Some(path),
                                from: root_name.clone(),
                                source: source_name(tree, source),
                            })
                        })
                        .as_ref()
                };
                if let Some(target) = target {
                    self.wanted_tables
                        .entry(target.key.clone())
                        .or_default()
                        .insert(table);
                    root_bindings.push(Binding {
                        occurrence: occurrence.id,
                        occurrence_name: occurrence.name.clone(),
                        target: target.clone(),
                        table,
                    });
                }
            }
        }
        self.occurrences.clear();
        for binding in &root_bindings {
            self.occurrences
                .entry(binding.target.key.clone())
                .or_default()
                .insert(binding.occurrence);
        }
        self.root_bindings = root_bindings.clone();
        // On demand, a data source naming no file this client opens is
        // reported once something reads one of its occurrences.
        let read = self.on_demand.then_some(&self.read);
        let mut warnings = unavailable_sources(catalog, tree, endpoint, &root_name, read);
        warnings.extend(variable_warnings.finish());
        // The data sources left untried, on demand: their occurrences wait.
        let mut deferred: BTreeSet<OsString> = BTreeSet::new();
        // Each file opened, by key: the opened file's own (`None`) and the
        // related files' (their index).
        let mut known: BTreeMap<OsString, Option<usize>> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.key.clone(), Some(index)))
            .collect();
        known.insert(self.root.clone(), None);
        let mut seen: BTreeSet<OsString> = known.keys().cloned().collect();
        let mut pending = Vec::new();
        let mut at = 0;
        let (mut found, mut opens) = (0, 0);
        let mut limited = false;
        discover(
            &root_bindings,
            &mut pending,
            &mut seen,
            &mut found,
            &mut limited,
        );
        for file in &self.files {
            discover(
                &file.bindings,
                &mut pending,
                &mut seen,
                &mut found,
                &mut limited,
            );
        }
        while at < pending.len() {
            // Files on a shared connection open one at a time, as do the
            // files a host is offered the opened file's account for, so
            // that it is sent once ([`Credentials::Offer`]).
            let parallel = if auth.as_ref().is_some_and(|auth| auth.connection.is_some())
                || other_hosts
                    .iter()
                    .any(|other| other.connection.is_some() || other.offered)
            {
                1
            } else {
                PARALLEL_LOADS
            };
            if cancel.pending() {
                self.close();
                return Err(closing());
            }
            if self.on_demand && stop() {
                // What is left waits until something reads it again.
                for (target, _) in &pending[at..] {
                    self.wanted.remove(&target.key);
                    deferred.insert(target.key.clone());
                }
                break;
            }
            // Each entry: the data source's target, the index of the path
            // being opened, and that file.
            let mut batch = Vec::new();
            let mut opening = BTreeSet::new();
            let mut later = Vec::new();
            while at < pending.len() && batch.len() < parallel {
                let (target, next) = pending[at].clone();
                at += 1;
                if let Some(warning) = self.failed.get(&target.key) {
                    warnings.push(warning.clone());
                    continue;
                }
                if self.local.is_some() && target.path.is_none() {
                    let warning = format!(
                        "{}: there is no such file beside “{}”.",
                        about(&target, &target.name),
                        target.from
                    );
                    warnings.push(warning.clone());
                    self.failed.insert(target.key, warning);
                    continue;
                }
                let without_credentials = |warnings: &mut Warnings, target: &Target| {
                    warnings.push(format!(
                        "{}: it was not opened; open this file again to open it.",
                        about(target, &target.name)
                    ));
                };
                if self.local.is_some() {
                    if auth.is_none() {
                        without_credentials(&mut warnings, &target);
                    } else if let Some(error) = self.remembered_refusal(auth.as_ref(), &target) {
                        let warning = format!("{}: {error}.", about(&target, &target.name));
                        warnings.push(warning.clone());
                        self.note_refused(&target, &target.key);
                        self.failed.insert(target.key, warning);
                    } else if opens == MAX_FILES {
                        // The same budget of Opens as on a host.
                        limited = true;
                    } else {
                        opens += 1;
                        batch.push((target.clone(), 0, target, Account::Opened));
                    }
                    continue;
                }
                // On a host, the data source's paths in their order, as
                // FileMaker Pro searches them: one the host answered it
                // does not have is passed over, and the first that names
                // a file the host has, or a file already open, is the
                // file. One the host would not open for another reason
                // (the account refused, in this open or earlier in the
                // session, refusals.rs) ends the search: the file is
                // there, and it gets no second Login.
                match self.next_path(&target, next, &known, &opening) {
                    // On demand, a file no read asked for is not opened
                    // yet, and nothing is said about it.
                    NextPath::Open(_) if self.on_demand && !self.wanted.contains(&target.key) => {
                        deferred.insert(target.key.clone());
                    }
                    NextPath::Open(index)
                        if self
                            .unreachable
                            .get(&target.paths[index].key())
                            .is_some_and(|(when, _)| {
                                when.elapsed() < self.retry_after.unwrap_or(RETRY_AFTER)
                            }) =>
                    {
                        // Its host could not be reached a moment ago: the
                        // data source waits, tried again later (#740); its
                        // warning is given below.
                        self.wanted.remove(&target.key);
                        deferred.insert(target.key.clone());
                    }
                    NextPath::Open(index) => {
                        let located = target.paths[index].clone();
                        let key = located.key();
                        let file = Target {
                            paths: vec![located.clone()],
                            name: located.name.clone(),
                            key: key.clone(),
                            path: None,
                            from: target.from.clone(),
                            source: target.source.clone(),
                        };
                        let account = match self.credentials_for(&file, auth.as_ref(), &other_hosts)
                        {
                            Credentials::Held(account) => account,
                            // The opened file's account, for that host
                            // only: kept there once a file admits it.
                            Credentials::Offer(root) => {
                                other_hosts.push(Auth {
                                    host: located.host.clone(),
                                    port: located.port,
                                    account: root.account.clone(),
                                    password: root.password.clone(),
                                    connection: None,
                                    offered: true,
                                });
                                Account::OtherHost(other_hosts.len() - 1)
                            }
                            // A rebuilt schema finds its files open
                            // already; a file still to open needs the
                            // credentials.
                            Credentials::Dropped => {
                                without_credentials(&mut warnings, &target);
                                continue;
                            }
                            // Another host's file that refused the
                            // opened file's account, or that is not
                            // offered it: the window asks for one, and
                            // the data source's search ends there
                            // rather than binding a later path's file
                            // (#976).
                            Credentials::Ask => {
                                let warning = format!(
                                    "{}: it is on another host ({}:{}), which did not \
                                         admit the account this file was opened with; sign in \
                                         to open it.",
                                    about(&file, &file.name),
                                    located.host,
                                    located.port,
                                );
                                warnings.push(warning.clone());
                                self.note_refused(&file, &target.key);
                                self.awaiting.insert(file.key.clone());
                                self.failed.insert(file.key, warning.clone());
                                self.failed.insert(target.key, warning);
                                continue;
                            }
                        };
                        let Some(file_auth) = account_of(&account, auth.as_ref(), &other_hosts)
                        else {
                            without_credentials(&mut warnings, &target);
                            continue;
                        };
                        // The opened file's account is offered to a host
                        // once: another of its files waits for the answer.
                        if file_auth.offered
                            && batch.iter().any(|(_, _, _, other)| *other == account)
                        {
                            later.push((target, index));
                            continue;
                        }
                        if let Some(error) = self.remembered_refusal(Some(file_auth), &file) {
                            let warning = format!("{}: {error}.", about(&file, &file.name));
                            warnings.push(warning.clone());
                            self.note_refused(&file, &target.key);
                            self.failed.insert(file.key, warning.clone());
                            self.failed.insert(target.key, warning);
                            continue;
                        }
                        if opens == MAX_FILES {
                            limited = true;
                            continue;
                        }
                        opens += 1;
                        opening.insert(key);
                        batch.push((target, index, file, account));
                    }
                    NextPath::Known(file) => {
                        if let Some(index) = file {
                            let key = self.files[index].key.clone();
                            self.carry_table_demand(&target.key, &key);
                        }
                        known.insert(target.key, file);
                    }
                    NextPath::Failed(file, warning) => {
                        // A file that refused the account ends this data
                        // source too: a sign-in to it resolves both.
                        if let Some((_, sources)) =
                            self.refused.iter_mut().find(|(t, _)| t.key == file)
                        {
                            sources.insert(target.key.clone());
                        }
                        warnings.push(warning.clone());
                        self.failed.insert(target.key, warning);
                    }
                    NextPath::Missing => {
                        let warning = not_hosted_warning(&target);
                        warnings.push(warning.clone());
                        self.failed.insert(target.key, warning);
                    }
                    // Its next path is being opened for another data
                    // source: it follows that one's answer.
                    NextPath::Wait(index) => later.push((target, index)),
                }
            }
            pending.extend(later);
            for (target, _, file, _) in &batch {
                self.carry_table_demand(&target.key, &file.key);
            }
            // Each file with the account of its host: only files with one
            // are in the batch.
            let keepalive = self.keepalive;
            let ca_scope = self.ca_scope.as_ref();
            let local = self.local.as_ref();
            let refusals = &self.refusals;
            // On demand, a file's open reads the field catalogs of the tables
            // a read asked for, the rest when first needed (#772).
            let (on_demand, wanted_tables) = (self.on_demand, &self.wanted_tables);
            let eager = |file: &Target| {
                on_demand.then(|| wanted_tables.get(&file.key).cloned().unwrap_or_default())
            };
            let results = load_batch(&batch, &|(_, _, file, account)| {
                let auth = account_of(account, auth.as_ref(), &other_hosts)
                    .ok_or_else(|| DataError::new("there is no account for its host"))?;
                let config = crate::scoped_config(config, ca_scope, &auth.host, auth.port);
                load(
                    &config,
                    auth,
                    local,
                    keepalive,
                    cancel,
                    refusals,
                    (file, eager(file).as_ref()),
                )
            });
            // In the order they were found, not the order they finished:
            // the same files import to the same ids.
            for ((target, index, file, account), result) in batch.into_iter().zip(results) {
                match result {
                    Ok(mut opened) => {
                        self.name_hosted(&mut opened, &file);
                        self.unreachable.remove(&file.key);
                        // Another host admitted the account: its next files
                        // open with it, in this file's session there.
                        if let Account::OtherHost(at) = account {
                            if let Some(other) = other_hosts.get_mut(at) {
                                other.offered = false;
                                if other.connection.is_none() {
                                    other.connection =
                                        opened.owner.borrow().session.as_ref().map(Session::share);
                                }
                            }
                        }
                        discover(
                            &opened.bindings,
                            &mut pending,
                            &mut seen,
                            &mut found,
                            &mut limited,
                        );
                        known.insert(opened.key.clone(), Some(self.files.len()));
                        known.insert(target.key, Some(self.files.len()));
                        self.files.push(opened);
                    }
                    Err(error) if self.local.is_none() && is_not_hosted(&error) => {
                        self.not_hosted.insert(file.key);
                        pending.push((target, index + 1));
                    }
                    // Its host could not be reached, before any Login: not
                    // failed for the session (#740). The data source waits
                    // (on demand, its occurrences wait for a read) and is
                    // tried again once a while has passed, the next schema
                    // build tries it otherwise.
                    Err(error) if is_unreachable(&error) => {
                        let warning = format!(
                            "{}: {error}; it is tried again when it is next needed.",
                            about(&file, &file.name)
                        );
                        self.unreachable
                            .insert(file.key, (std::time::Instant::now(), warning));
                        self.wanted.remove(&target.key);
                        deferred.insert(target.key);
                    }
                    Err(error) => {
                        // Its Login refused the credentials (`load` noted
                        // it): another account may open it, and one typed
                        // for its host opens it with that host's other
                        // files waiting for one.
                        let held = account_of(&account, auth.as_ref(), &other_hosts);
                        let refused = self.remembered_refusal(held, &file).is_some();
                        let warning = match (held, file.paths.first()) {
                            (Some(held), Some(located)) if refused && held.offered => format!(
                                "{}: it is on another host ({}:{}), which did not admit the \
                                 account this file was opened with; sign in to open it.",
                                about(&file, &file.name),
                                located.host,
                                located.port,
                            ),
                            _ => format!("{}: {error}.", about(&file, &file.name)),
                        };
                        warnings.push(warning.clone());
                        if refused {
                            self.note_refused(&file, &target.key);
                            if held.is_some_and(|held| held.offered) {
                                self.awaiting.insert(file.key.clone());
                            }
                        }
                        self.failed.insert(file.key, warning.clone());
                        self.failed.insert(target.key, warning);
                    }
                }
            }
        }
        // No later schema import needs the opened file's credentials (the
        // provider keeps them for its scripts' opens, and lends them,
        // [`Self::lend`]). The accounts other hosts admitted stay until the
        // file closes or reconnects, each for its own host only: a script's
        // open there, a later schema, or on demand a later read that opens
        // another file there, sends it first. An offer no file admitted
        // goes.
        drop(auth);
        self.other_hosts = other_hosts
            .into_iter()
            .filter(|other| !other.offered)
            .map(|other| Auth {
                connection: None,
                ..other
            })
            .collect();
        if cancel.pending() {
            self.close();
            return Err(closing());
        }

        // The tables of open files a read asked for whose field catalogs
        // were not read yet: read now, so the import below has them.
        let wanted = &self.wanted_tables;
        for file in &mut self.files {
            if let Some(tables) = wanted.get(&file.key) {
                file.read_tables(tables, &|| stop() || cancel.pending())?;
            }
        }
        if cancel.pending() {
            self.close();
            return Err(closing());
        }
        self.routes.clear();
        self.owner_plans.get_mut().clear();
        // Reserve all files before importing any: growth in an earlier
        // file must not consume a later file's published IDs.
        for map in &self.import_maps {
            schema.reserve_imported_ids(map);
        }
        let mut maps = Vec::with_capacity(self.files.len());
        for (index, file) in self.files.iter().enumerate() {
            let map = schema.import_owned_file_schema_with_map(
                &file.schema,
                file.path
                    .as_deref()
                    .map(om_data::file_refs::local_file_path)
                    .or_else(|| file.hosted_path.clone()),
                Some(file.identity.clone()),
                self.import_maps.get(index).cloned().unwrap_or_default(),
            );
            for (&own, &client) in &map.tables {
                // A table the file's occurrences name but the account
                // cannot read has an id and no definition: nothing to read.
                if file.schema.table(own).is_some() {
                    self.routes.insert(
                        client,
                        TableRoute {
                            file: index,
                            table: own,
                        },
                    );
                }
            }
            warnings.extend(file.warnings.lines.iter().cloned());
            warnings.omitted = warnings.omitted.saturating_add(file.warnings.omitted);
            maps.push(map);
        }
        // What [`Self::want`] maps an imported occurrence back to.
        self.imported = maps
            .iter()
            .enumerate()
            .flat_map(|(index, map)| {
                map.occurrences
                    .iter()
                    .map(move |(&own, &client)| (client, (index, own)))
            })
            .collect();
        // Occurrences whose table could not be bound, by the file they are
        // in, the file the data source found and that file's table: one
        // line each, naming its occurrences.
        let names: Vec<String> = self.files.iter().map(|file| file.name.clone()).collect();
        let mut unbound: Vec<(Unbound, Vec<String>)> = Vec::new();
        let mut more_unbound = 0usize;
        let mut bind = |bindings: &[Binding], owner: Option<&ImportedFileMap>, from: &str| {
            for binding in bindings {
                // A file that is not open leaves its occurrences unresolved;
                // one not tried yet, on demand, leaves them waiting for it.
                let Some(target) = known.get(&binding.target.key) else {
                    if deferred.contains(&binding.target.key) {
                        let occurrence = match owner {
                            None => Some(binding.occurrence),
                            Some(map) => map.occurrences.get(&binding.occurrence).copied(),
                        };
                        if let Some(occurrence) = occurrence {
                            schema.set_pending(occurrence);
                        }
                    }
                    continue;
                };
                // The file the data source found: not its first path, which
                // may be one the host does not have.
                let (file, table) = match target {
                    None => (root_name.clone(), Some(binding.table)),
                    Some(index) => (
                        names[*index].clone(),
                        maps[*index].tables.get(&binding.table).copied(),
                    ),
                };
                let occurrence = match owner {
                    None => Some(binding.occurrence),
                    Some(map) => map.occurrences.get(&binding.occurrence).copied(),
                };
                let bound = match (occurrence, table) {
                    (Some(occurrence), Some(table)) => {
                        schema.bind_external_occurrence(occurrence, table)
                    }
                    _ => false,
                };
                if !bound {
                    let key = Unbound {
                        from: from.to_owned(),
                        file,
                        table: binding.table,
                        // As the host sent the file: no base table of that
                        // key, or one without a definition. Why is not
                        // known here.
                        missing: table.is_none(),
                    };
                    match unbound.iter().position(|(k, _)| *k == key) {
                        Some(at) => unbound[at].1.push(binding.occurrence_name.clone()),
                        None if unbound.len() < MAX_WARNINGS => {
                            unbound.push((key, vec![binding.occurrence_name.clone()]))
                        }
                        None => more_unbound = more_unbound.saturating_add(1),
                    }
                }
            }
        };
        bind(&root_bindings, None, &root_name);
        for (index, file) in self.files.iter().enumerate() {
            bind(&file.bindings, Some(&maps[index]), &file.name);
        }
        for (key, occurrences) in unbound {
            warnings.push(key.warning(&occurrences));
        }
        // Each file whose host could not be reached and is not open yet,
        // whichever data source or read waits for it (#740).
        for (key, (_, warning)) in &self.unreachable {
            if !known.contains_key(key) {
                warnings.push(warning.clone());
            }
        }
        warnings.omitted = warnings.omitted.saturating_add(more_unbound);
        if limited {
            warnings.prepend(format!("This file references more than {MAX_FILES} related files; the rest were not opened."));
        }
        self.warnings = warnings.finish();
        self.variable_targets
            .extend(variable_targets.into_iter().filter_map(|(source, target)| {
                let target = target?;
                known.contains_key(&target.key).then_some((source, target))
            }));
        // Added to: a data source resolved stays so until the file closes,
        // also when a later import stops part way. Related files are only
        // added to until then, so their indexes hold.
        self.resolved.extend(known);
        self.import_maps = maps;
        Ok(())
    }
}

/// Occurrences of file `from` whose table, `table` of file `file` (the
/// one their data source found), could not be bound: `missing` when that
/// file as the host sent it has no definition of the table.
#[derive(PartialEq)]
struct Unbound {
    from: String,
    file: String,
    table: u64,
    missing: bool,
}

impl Unbound {
    /// The warning for `occurrences` (at least one), naming up to three.
    fn warning(&self, occurrences: &[String]) -> String {
        let named = quoted_names(occurrences, occurrences.len());
        let (from, file) = (&self.from, &self.file);
        match (occurrences.len() == 1, self.missing) {
            (true, true) => format!(
                "Occurrence {named} of “{from}” shows a table that “{file}” does not have as \
                 the host sent it; the occurrence's table is unavailable."
            ),
            (false, true) => format!(
                "Occurrences {named} of “{from}” show a table that “{file}” does not have as \
                 the host sent it; their table is unavailable."
            ),
            (true, false) => format!(
                "Occurrence {named} of “{from}” could not be bound to its table in “{file}”; \
                 it shows no data."
            ),
            (false, false) => format!(
                "Occurrences {named} of “{from}” could not be bound to their table in \
                 “{file}”; they show no data."
            ),
        }
    }
}

/// What [`ExternalFiles::next_path`] found for a data source.
enum NextPath {
    /// Open this path (its index).
    Open(usize),
    /// The path names a file already open: the opened file's own (`None`)
    /// or a related file (its index).
    Known(Option<usize>),
    /// The path names a file that could not be opened (its key), for this
    /// warning.
    Failed(OsString, String),
    /// The host has none of its paths.
    Missing,
    /// Another data source is opening this path (its index) now.
    Wait(usize),
}

/// Whose account a related file is sent: the opened file's, or the one
/// signed in to on its other host (`ExternalFiles::other_hosts`, by index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Account {
    Opened,
    OtherHost(usize),
}

/// The credentials of `account`: the opened file's (`root`) or another
/// host's (`others`).
fn account_of<'a>(
    account: &Account,
    root: Option<&'a Auth>,
    others: &'a [Auth],
) -> Option<&'a Auth> {
    match account {
        Account::Opened => root,
        Account::OtherHost(at) => others.get(*at),
    }
}

/// What [`ExternalFiles::credentials_for`] found for a related file.
enum Credentials<'a> {
    /// This account.
    Held(Account),
    /// The opened file's account (these credentials), offered to the
    /// file's other host or port once, as FileMaker Pro offers it before
    /// it asks (network-protocol.md 5.5): no file there refused it in this
    /// session, and none admitted another account.
    Offer(&'a Auth),
    /// The opened file's host's account, which went with the first schema.
    Dropped,
    /// None: the file is on another host or port than the opened file's,
    /// which refused the opened file's account (or it is not held), and no
    /// account was signed in to there.
    Ask,
}

impl ExternalFiles {
    /// The account related file `file` is opened with: on this computer or
    /// the opened file's host and port, the opened file's (`root`, while
    /// held); on another host or port, one that host admitted (typed for
    /// it, or the opened file's, `others`), else the opened file's offered
    /// there once, never through another host's connection.
    fn credentials_for<'a>(
        &self,
        file: &Target,
        root: Option<&'a Auth>,
        others: &[Auth],
    ) -> Credentials<'a> {
        if self.local.is_some() || self.on_opened_host(file) {
            return root.map_or(Credentials::Dropped, |_| Credentials::Held(Account::Opened));
        }
        let Some(located) = file.paths.first() else {
            return Credentials::Ask;
        };
        let refused_there = |auth: &Auth| {
            self.refusals
                .lock()
                .expect("login refusals")
                .refused_on(&located.host, located.port, &auth.account, &auth.password)
                .is_some()
        };
        if let Some(at) = others
            .iter()
            .position(|auth| located.on((&auth.host, auth.port)))
        {
            // An offer a file there refused: the window asks.
            if others[at].offered && refused_there(&others[at]) {
                return Credentials::Ask;
            }
            return Credentials::Held(Account::OtherHost(at));
        }
        match root {
            Some(root) if !refused_there(root) => Credentials::Offer(root),
            _ => Credentials::Ask,
        }
    }

    /// Another handle on the session a related file open on `host` at
    /// `port` has, when one is open there.
    pub fn session_on(&self, host: &str, port: u16) -> Option<Session> {
        self.files.iter().find_map(|file| match &file.identity {
            om_data::file_refs::FileIdentity::Hosted {
                host: there,
                port: at,
                ..
            } if *at == port && there.eq_ignore_ascii_case(host) => file
                .owner
                .try_borrow()
                .ok()?
                .session
                .as_ref()
                .map(Session::share),
            _ => None,
        })
    }

    /// The refusal the session remembers of `file` for these credentials,
    /// if it refused them before (refusals.rs): it is not sent them again.
    fn remembered_refusal(&self, auth: Option<&Auth>, file: &Target) -> Option<DataError> {
        let auth = auth?;
        self.refusals.lock().expect("login refusals").refused(
            &refusal_key(auth, file),
            &auth.account,
            &auth.password,
        )
    }

    /// What data source `target` does next with its paths from index
    /// `next` on ([`NextPath`]): paths the host answered it does not have
    /// are passed over, and the first other path decides. `known` holds
    /// the open files by key, `opening` the paths being opened now.
    fn next_path(
        &self,
        target: &Target,
        next: usize,
        known: &BTreeMap<OsString, Option<usize>>,
        opening: &BTreeSet<OsString>,
    ) -> NextPath {
        for (index, located) in target.paths.iter().enumerate().skip(next) {
            let key = located.key();
            if let Some(file) = known.get(&key) {
                return NextPath::Known(*file);
            }
            if self.not_hosted.contains(&key) {
                continue;
            }
            if let Some(warning) = self.failed.get(&key) {
                return NextPath::Failed(key, warning.clone());
            }
            if opening.contains(&key) {
                return NextPath::Wait(index);
            }
            return NextPath::Open(index);
        }
        NextPath::Missing
    }
}

/// The warning for a data source none of whose paths the host has.
/// `Get ( FilePath )` of related file `name` (as the file is named, without
/// its extension) at `located`, when that is not the opened file's host and
/// port (`opened`): `fmnet:/host/Name.fmp12` with its host as the data
/// source names it, and `:port` when the port is not the opened file's
/// (`file_refs::hosted_file_path`). `None` on the opened file's host and
/// port, whose files keep the path derived beside the opened file's.
fn hosted_path(opened: Option<(&str, u16)>, located: &Located, name: &str) -> Option<String> {
    if opened.is_some_and(|endpoint| located.on(endpoint)) {
        return None;
    }
    let port = (opened.map(|(_, port)| port) != Some(located.port)).then_some(located.port);
    Some(om_data::file_refs::hosted_file_path(
        &located.host,
        port,
        name,
    ))
}

fn not_hosted_warning(target: &Target) -> String {
    match target.paths.as_slice() {
        [_] => format!(
            "{}: the host does not have it.",
            about(target, &target.name)
        ),
        paths => format!(
            "{}: the host has none of the {} files its data source lists ({}).",
            about(target, &target.name),
            paths.len(),
            paths
                .iter()
                .map(|path| path.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// How a warning names related file `file`: with the file whose data
/// source names it, and that data source by name when it can be read.
/// Every line says which file it is about, so lines about different files
/// never read the same.
fn about(target: &Target, file: &str) -> String {
    match &target.source {
        Some(source) => format!(
            "Related file “{file}” (data source “{source}” of “{}”)",
            target.from
        ),
        None => format!("Related file “{file}” (named by “{}”)", target.from),
    }
}

/// An Open the host refused with `status` ([`Session::refused_open`]), or
/// that failed before it answered, in words: the statuses
/// network-protocol.md 5.5 and 5.6.4 name, others by their number.
fn open_refused(status: Option<u16>, error: om_session::SessionError) -> DataError {
    match status {
        Some(status) if NOT_HOSTED.contains(&status) => not_hosted(),
        Some(0x4fb3) => DataError::new("the host cannot open it"),
        Some(1638) => DataError::new("the host is not allowing new connections"),
        Some(status) => DataError::new(format!(
            "the host would not open it (Open status 0x{status:04x})"
        )),
        None => failure("open the file", error),
    }
}

/// The error for an Open the host answered with a [`NOT_HOSTED`] status.
fn not_hosted() -> DataError {
    DataError::with_code(100, "the host does not have this file")
}

/// Whether `error` is [`not_hosted`]'s.
fn is_not_hosted(error: &DataError) -> bool {
    error.code == Some(100) && error.message == not_hosted().message
}

/// How long a related file whose host could not be reached waits before a
/// read tries it again (#740) [I: OpenMaker's choice; whether and when
/// FileMaker Pro tries again is unmeasured]: long enough that a window
/// painting its fields does not wait on the host each time.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// A related file's host could not be reached: no connection to it
/// (host lookup, connect or TLS). The operating system's own words for it
/// are left out of the alert.
fn unreachable() -> DataError {
    DataError::new("OpenMaker could not connect to the host")
}

/// Whether `error` is [`unreachable`]'s: the transport failed, before any
/// Open or Login (#740).
fn is_unreachable(error: &DataError) -> bool {
    error.code.is_none() && error.message == unreachable().message
}

/// Why opening the related files stopped: the file is being closed.
fn closing() -> DataError {
    DataError::new("The file is closing; its related files were not opened.")
}

impl Drop for ExternalFiles {
    fn drop(&mut self) {
        self.close();
    }
}

/// `load` of each of `files` (at most [`PARALLEL_LOADS`]) on a thread of
/// its own, the results in the order of `files`. Each connection is made
/// inside its thread and comes back when the thread ends; the credentials
/// are only borrowed for the batch.
fn load_batch<F: Sync, T: Send>(
    files: &[F],
    load: &(impl Fn(&F) -> Result<T, DataError> + Sync),
) -> Vec<Result<T, DataError>> {
    debug_assert!(files.len() <= PARALLEL_LOADS);
    std::thread::scope(|scope| {
        let workers: Vec<_> = files
            .iter()
            .map(|file| {
                std::thread::Builder::new()
                    .name("om-related-file".into())
                    .spawn_scoped(scope, move || {
                        let loaded = load(file);
                        (loaded, om_session::record_requests_sent())
                    })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| match worker {
                // What the load sent counts as its caller's (#152). A load
                // that panicked counts as having sent something.
                Ok(worker) => match worker.join() {
                    Ok((loaded, sent)) => {
                        om_session::add_record_requests_sent(sent);
                        loaded
                    }
                    Err(_) => {
                        om_session::add_record_requests_sent(1);
                        Err(DataError::new("Opening the file failed unexpectedly."))
                    }
                },
                Err(error) => Err(failure("start opening the file", error)),
            })
            .collect()
    })
}

/// Select only the root catalogs the related loader already reads, using
/// op92's existing sibling selection and paging (network-protocol.md 5.6.6b).
/// Field catalogs and the account's narrow security paths stay separate.
fn download_related_catalogs(
    session: &mut Session,
    status: Option<Download>,
    cancel: &CloseRequests,
) -> Result<Vec<Download>, DataError> {
    let mut keys: Vec<Vec<u8>> = RELATED_CATALOGS.into_iter().map(key_bytes).collect();
    if status.is_none() {
        keys.insert(0, key_bytes(crate::STATUS));
    }
    let refs: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
    let reply = session
        .download_list(&[], &refs, 0)
        .map_err(|e| failure("download related file catalogs", e))?;
    // A restricted account cannot read the root parent, which can contain
    // credential material, even though each selected catalog is readable.
    // Keep the server's guard and use the original narrow reads on 200.
    let mut downloads = if reply.status == 200 {
        let mut downloads = Vec::new();
        for key in &keys {
            if cancel.pending() {
                return Err(closing());
            }
            let data = if key == &[3] {
                Some(download(session, &[3])?)
            } else {
                download_node(session, vec![key.clone()])?
            };
            if let Some(data) = data {
                downloads.push(data);
            }
        }
        downloads
    } else {
        catalogs_from_list(reply, &keys)?
    };
    if let Some(status) = status {
        downloads.push(status);
    }
    downloads.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(downloads)
}

/// Keep the same per-path downloads as individual op90s. Missing optional
/// catalogs are omitted by op92; the tables catalog is still required.
fn catalogs_from_list(
    reply: om_protocol::DownloadListReply,
    keys: &[Vec<u8>],
) -> Result<Vec<Download>, DataError> {
    if reply.status != 0 {
        return Err(DataError::new(format!(
            "Server catalog list failed (lock error 0x{:04x}).",
            reply.status
        )));
    }
    let mut reader = SlotReader::new(&reply.slots);
    let tree = KeyTree::from_slots(reader.by_ref())
        .map_err(|e| failure("decode related file catalogs", e))?;
    if !reader.root_closed() || !reader.path().is_empty() {
        return Err(DataError::new("The server catalog list is incomplete."));
    }
    if tree.root.child(&[3]).is_none() {
        return Err(DataError::new("The server has no [3]."));
    }
    tree.root
        .children
        .iter()
        .map(|node| {
            if !keys.iter().any(|key| key.as_slice() == node.segment.0) {
                return Err(DataError::new(
                    "The server returned an unrequested catalog.",
                ));
            }
            if !node.push && node.children.is_empty() {
                return Err(DataError::new("The server catalog is not a node."));
            }
            Ok(Download {
                path: vec![node.segment.0.to_vec()],
                slots: node
                    .to_wire()
                    .ok_or_else(|| DataError::new("The server catalog cannot be decoded."))?,
            })
        })
        .collect()
}

/// Open related file `target` on the opened file's host (on this
/// computer: through its host in the family, `local`) and read what its
/// calculations need. Once a Close waits in `cancel` it stops before the
/// next step that waits for the host, the Login above all.
fn load(
    config: &NativeConfig,
    auth: &Auth,
    local: Option<&LocalFiles>,
    keepalive: Option<Duration>,
    cancel: &CloseRequests,
    refusals: &crate::refusals::Shared,
    request: (&Target, Option<&BTreeSet<u64>>),
) -> Result<RelatedFile, DataError> {
    let (target, eager) = request;
    let go_on = || {
        if cancel.pending() {
            Err(closing())
        } else {
            Ok(())
        }
    };
    go_on()?;
    // A file on this computer: the family's host of it, shared with the
    // family's other sessions of the file.
    let lease = match (local, &target.path) {
        (Some(local), Some(path)) => Some(local.hosts.acquire(path, &config.client_cert_pem)?),
        (Some(_), None) => return Err(DataError::with_code(100, "it is missing")),
        (None, _) => None,
    };
    let (host, port, ca_pem, machine, requested) = match &lease {
        Some(lease) => (
            "127.0.0.1".to_owned(),
            lease.port(),
            lease.certificate_pem().to_vec(),
            // No client license goes to a file OpenMaker opens itself.
            om_protocol::MachineInfo {
                install_code: String::new(),
                ..config.machine.clone()
            },
            lease.file_name().to_owned(),
        ),
        None => (
            auth.host.clone(),
            auth.port,
            config.additional_ca_pem.clone(),
            config.machine.clone(),
            hosted_name(&target.name),
        ),
    };
    let connect = || {
        Session::connect_verified(
            &host,
            port,
            &config.client_cert_pem,
            &config.client_key_pem,
            &config.callback_host,
            &machine,
            &ca_pem,
        )
        // The operating system's own words for it are left out of the alert.
        .map_err(|_| unreachable())
    };
    // On the connection of a file open on that host: no host lookup, TLS
    // handshake or Connect of its own. A file open there already for the
    // same account and password (another file's related file, a desktop
    // file) is held with it, with no Open or Login of its own; one held for
    // another account, or being opened, opens on a connection of its own,
    // since a second Open on one connection would replace it (#1011).
    let admission = crate::refusals::admission(&auth.account, &auth.password);
    let (mut session, mut joined) = match (&lease, &auth.connection) {
        (None, Some(connection)) => {
            let mut handle = connection.share();
            match handle.claim_file(&requested, &admission) {
                Ok(om_session::Claim::Joined(login)) => (handle, Some(login)),
                Ok(om_session::Claim::Open) => (handle, None),
                Err(_) => (connect()?, None),
            }
        }
        _ => (connect()?, None),
    };
    session.set_keepalive(keepalive);
    // The file's node `[2]` as read before Login, when this handle logged in
    // (a file joined on a shared connection logs in no more).
    let mut status = None;
    let loaded = (|| {
        go_on()?;
        let login = match joined.take() {
            Some(login) => login,
            None => {
                session
                    .open_file(&host, &requested)
                    .map_err(|e| open_refused(session.refused_open(), e))?;
                go_on()?;
                let (creation, file_node) = session
                    .fetch_status()
                    .map_err(|e| failure("read the login challenge", e))?;
                go_on()?;
                let login = session
                    .login(
                        &auth.account,
                        &auth.password,
                        creation,
                        &crate::login_environment(&host, &requested),
                    )
                    .map_err(|e| failure("log in", e))?;
                // The file's node `[2]`, read for the challenge, is the one
                // its schema reads: FileMaker Pro 26 reads it once, before
                // Login (network-protocol.md 5.6.9b), so it is kept.
                status = node_from_reply(vec![key_bytes(crate::STATUS)], file_node)?;
                // A refused Login is remembered for the session (refusals.rs).
                let refused = |error: DataError| {
                    refusals.lock().expect("login refusals").record(
                        refusal_key(auth, target),
                        &auth.account,
                        &auth.password,
                        &error,
                    );
                    error
                };
                if !login.is_success() && lease.is_some() {
                    // Its own accounts refused it, as File > Open would have.
                    return Err(refused(DataError::with_code(
                        i32::from(login.error_code),
                        format!(
                            "the file's accounts did not admit “{}” (FileMaker error {})",
                            auth.account, login.error_code
                        ),
                    )));
                }
                if login.error_code == om_server::LOGIN_TOO_MANY_ATTEMPTS {
                    return Err(DataError::with_code(
                        i32::from(login.error_code),
                        "the host refused the login after too many failed attempts; wait a few \
                 minutes, then try again",
                    ));
                }
                if !login.is_success() {
                    // The code the first file's own refusal gives (`open_at`), which
                    // a script step that opens this file later reports.
                    let (code, message) = match login.error_code {
                        0xd0 | 0x20d0 => (
                            3,
                            format!(
                                "the host requires a password change or another way of signing in \
                         for the account “{}” before it opens this file (Login status \
                         0x{:04x})",
                                auth.account, login.error_code
                            ),
                        ),
                        _ => (
                            212,
                            format!(
                                "the host did not accept the account “{}” and its password for it \
                         (FileMaker error {})",
                                auth.account, login.error_code
                            ),
                        ),
                    };
                    return Err(refused(DataError::with_code(code, message)));
                }
                go_on()?;
                session
                    .request_notifications()
                    .map_err(|e| failure("subscribe to file changes", e))?;
                // Another handle on the connection opening the file for the same
                // account and password holds it with this one.
                session
                    .admit_file(admission.clone())
                    .map_err(|e| failure("share the file", e))?;
                login
            }
        };
        go_on()?;
        let mut downloads = download_related_catalogs(&mut session, status.take(), cancel)?;
        let base = Catalog::from_tree(&download_tree(&downloads)?)
            .map_err(|e| failure("decode the file's tables", e))?;
        // Every table's field catalog, or on demand (`eager`) only those a
        // read asked for: the others are marked unread below, as FileMaker
        // Pro reads a related table's catalog when first needed
        // (network-protocol.md 5.5).
        let mut unread = Vec::new();
        for table in &base.tables {
            go_on()?;
            if eager.is_none_or(|eager| eager.contains(&table.id)) {
                downloads.push(download(&mut session, &[table.id, 3])?);
            } else {
                unread.push(table.id);
            }
        }
        // A key tree is built in key order.
        downloads.sort_by(|a, b| a.path.cmp(&b.path));
        let tree = download_tree(&downloads)?;
        let catalog =
            Catalog::from_tree(&tree).map_err(|e| failure("decode the file's fields", e))?;
        let name = om_data::file_refs::file_stem(&requested).to_owned();
        let mut schema = crate::calc::calc_schema(&tree, &catalog, &name);
        for &table in &unread {
            schema.set_fields_pending(table);
        }
        go_on()?;
        schema.authenticated_account = lease
            .as_ref()
            .map(|host| host.authenticated_account(&login.result_key))
            .transpose()
            .map_err(crate::local_unreadable)?
            .unwrap_or_default();
        let privileges =
            crate::privileges::read_current_account(&mut session, &login.result_key, cancel);
        // Hosted owners use the set of their own authenticated account,
        // decoded from the same reply as its privilege names. Local owners
        // already have their host's authoritative account context.
        if lease.is_none() {
            schema.authenticated_account.set_access = privileges.access;
        }
        schema.withhold_hidden_layouts();
        schema.authenticated_privileges = privileges.account;
        go_on()?;
        // Its own related files are beside it.
        let folder = lease.as_ref().and_then(|lease| lease.path().parent());
        // On the host: the one the user opened, whose `fmnet` paths it
        // follows too.
        let endpoint = lease.is_none().then_some((auth.host.as_str(), auth.port));
        let bindings = bindings(&catalog, &tree, folder, endpoint, &name);
        let warnings = unavailable_sources(&catalog, &tree, endpoint, &name, None);
        let recovery_creation = tree.internal_creation().ok().flatten();
        Ok((
            name,
            schema,
            bindings,
            warnings,
            catalog,
            downloads,
            privileges.principal,
            recovery_creation,
        ))
    })();
    match loaded {
        Ok((
            name,
            schema,
            bindings,
            warnings,
            catalog,
            downloads,
            principal,
            recovery_creation,
        )) => {
            let mut owner = crate::NativeProvider::new(config.clone());
            owner.session = Some(session);
            owner.catalog = catalog.clone();
            owner.recovery_creation = recovery_creation;
            owner.schema_downloads = downloads;
            owner.account_name = auth.account.clone();
            owner.account_info = schema.authenticated_account.clone();
            owner.calc.file_name = name.clone();
            owner.calc.host_name = match &lease {
                // A local file's host is this computer.
                Some(_) => om_data::host::machine::Machine::current().host_name.clone(),
                None => auth.host.clone(),
            };
            owner.calc.schema = Some(std::sync::Arc::new(schema.clone()));
            // download_related_catalogs succeeded for all selected catalogs;
            // unlike Root, this owner keeps them in schema_downloads already.
            owner.calc.recovery_catalogs = Some(Vec::new());
            owner.privileges.account = schema.authenticated_privileges.clone();
            owner.privileges.principal = principal;
            let path = lease.as_ref().map(|lease| lease.path().to_path_buf());
            owner.calc.file_path = path.as_deref().map(om_data::file_refs::local_file_path);
            // The owner's writes are refused as the file's own window
            // refuses them when it is read-only, and its lease goes with it.
            owner.local = lease;
            if let Some(local) = local {
                owner.local_hosts = Arc::clone(&local.hosts);
            }
            let identity = match (&path, target.paths.first()) {
                (Some(path), _) => om_data::file_refs::FileIdentity::local(path),
                (None, Some(located)) => om_data::file_refs::FileIdentity::hosted(
                    &located.host,
                    located.port,
                    &located.name,
                ),
                (None, None) => om_data::file_refs::FileIdentity::hosted(&host, port, &requested),
            };
            let tables = owner.catalog.tables.clone();
            Ok(RelatedFile {
                key: target.key.clone(),
                path,
                hosted_path: None,
                identity,
                name,
                owner: RefCell::new(owner),
                schema,
                tables,
                bindings,
                warnings,
            })
        }
        Err(error) => {
            let _ = session.disconnect();
            Err(error)
        }
    }
}

/// The file's occurrences of other files' tables whose data source names
/// a file this client opens ([`source_files`]). For a file on this
/// computer, `folder` is its folder: the file is the first of the source's
/// relative alternatives there ([`locate`]). On a host, `endpoint` (the
/// host and port the user opened), the target lists the source's paths to
/// it, in their order, for [`ExternalFiles::hydrate`] to try. `from` is
/// the file's name, for warnings.
fn bindings(
    catalog: &Catalog,
    tree: &KeyTree<'_>,
    folder: Option<&Path>,
    endpoint: Option<(&str, u16)>,
    from: &str,
) -> Vec<Binding> {
    let mut targets: BTreeMap<u64, Option<Target>> = BTreeMap::new();
    catalog
        .table_occurrences
        .iter()
        .filter_map(|occurrence| {
            let (source, table) = occurrence.reference().filter(|(s, _)| *s != 0)?;
            let target = targets
                .entry(source)
                .or_insert_with(|| {
                    let located = source_files(tree, source, endpoint).paths();
                    let first = located.first()?.name.clone();
                    let mut target = match folder {
                        Some(folder) => {
                            let names: Vec<String> =
                                located.into_iter().map(|path| path.name).collect();
                            locate(folder, &names)
                        }
                        None => {
                            // Each file once, in the order listed.
                            let mut keys = BTreeSet::new();
                            let paths: Vec<Located> = located
                                .into_iter()
                                .filter(|path| keys.insert(path.key()))
                                .take(MAX_PATHS)
                                .collect();
                            Target {
                                key: paths_key(&paths),
                                name: first,
                                path: None,
                                paths,
                                from: String::new(),
                                source: None,
                            }
                        }
                    };
                    target.from = from.to_owned();
                    target.source = source_name(tree, source);
                    Some(target)
                })
                .clone()?;
            Some(Binding {
                occurrence: occurrence.id,
                occurrence_name: occurrence.name.clone(),
                target,
                table,
            })
        })
        .collect()
}

/// File `name` beside a file on this computer: no host.
fn beside(name: &str) -> Located {
    Located {
        host: String::new(),
        port: 0,
        name: name.to_owned(),
    }
}

/// The file the relative alternatives `names` (in their order) name in
/// `folder`: the first that is a file there, by its path with links
/// followed. When none is, the first, missing.
fn locate(folder: &Path, names: &[String]) -> Target {
    for name in names {
        let candidate = folder.join(hosted_name(name));
        if let Ok(path) = std::fs::canonicalize(&candidate) {
            if path.is_file() {
                return Target {
                    name: name.clone(),
                    key: path.as_os_str().to_owned(),
                    path: Some(path),
                    paths: vec![beside(name)],
                    from: String::new(),
                    source: None,
                };
            }
        }
    }
    let name = names.first().cloned().unwrap_or_default();
    Target {
        key: folder.join(hosted_name(&name)).into_os_string(),
        paths: vec![beside(&name)],
        name,
        path: None,
        from: String::new(),
        source: None,
    }
}

/// A warning for each data source the file `from`'s occurrences use that
/// names no file this client opens: one that lists only path forms it does
/// not follow (another host, a folder or volume, or on this computer
/// anything but a plain name), or one with no list of paths to open (none,
/// an empty one, or one that does not decode).
/// Their occurrences stay unresolved.
///
/// A data source is named by its name. One without a name that reads as
/// one is named by the occurrences that use it, which FileMaker Pro's
/// relationships graph shows, and is said to be missing from the file's
/// data sources `[32, 5]` as the host sent them, there with neither a name
/// nor paths, or there with a name that is empty or does not decode (#761:
/// why any of these happens is not known).
fn unavailable_sources(
    catalog: &Catalog,
    tree: &KeyTree<'_>,
    endpoint: Option<(&str, u16)>,
    from: &str,
    read: Option<&BTreeSet<u64>>,
) -> Warnings {
    let local = endpoint.is_none();
    // Per data source, the first occurrences that use it (as many as a
    // line names) and how many do: the names are the catalog's, borrowed.
    // With `read`, only the data sources one of whose occurrences was read.
    let mut sources: BTreeMap<u64, (Vec<&str>, usize)> = BTreeMap::new();
    let wanted: Option<BTreeSet<u64>> = read.map(|read| {
        catalog
            .table_occurrences
            .iter()
            .filter(|occurrence| read.contains(&occurrence.id))
            .filter_map(|occurrence| occurrence.reference().map(|(source, _)| source))
            .collect()
    });
    for occurrence in &catalog.table_occurrences {
        if let Some((source, _)) = occurrence
            .reference()
            .filter(|(source, _)| *source != 0)
            .filter(|(source, _)| wanted.as_ref().is_none_or(|w| w.contains(source)))
        {
            let (shown, total) = sources.entry(source).or_default();
            if shown.len() < NAMES_SHOWN {
                shown.push(&occurrence.name);
            }
            *total = total.saturating_add(1);
        }
    }
    let mut warnings = Warnings::default();
    for (source, (occurrences, total)) in sources {
        let paths = source_files(tree, source, endpoint);
        let name = read_source_name(tree, source);
        let listed = tree.at(&[DATA_SOURCES, 5, source]).is_some();
        let used_by = || quoted_names(&occurrences, total);
        let subject = match &name {
            SourceName::Named(name) => {
                format!("Data source “{}” of “{from}”", shown_name(name))
            }
            SourceName::Empty => {
                format!(
                    "A data source of “{from}” with an empty name, used by {},",
                    used_by()
                )
            }
            SourceName::Unreadable => format!(
                "A data source of “{from}” whose name OpenMaker cannot read, used by {},",
                used_by()
            ),
            SourceName::Absent => format!("A data source of “{from}” used by {}", used_by()),
        };
        match paths {
            SourcePaths::Files(_) | SourcePaths::Variable(_) => {}
            SourcePaths::Absent if !listed => warnings.push(format!(
                "{subject} is not among the file's data sources as the host sent them, so its \
                 tables are unavailable."
            )),
            SourcePaths::Absent if matches!(name, SourceName::Absent) => warnings.push(format!(
                "{subject} has neither a name nor a list of file paths, so its tables are \
                 unavailable."
            )),
            SourcePaths::Absent => warnings.push(format!(
                "{subject} has no list of file paths, so its tables are unavailable."
            )),
            SourcePaths::Empty => warnings.push(format!(
                "{subject} lists no file paths, so its tables are unavailable."
            )),
            SourcePaths::Malformed => warnings.push(format!(
                "{subject} has a list of file paths OpenMaker cannot read, so its tables are \
                 unavailable."
            )),
            SourcePaths::NoneFollowed if local => warnings.push(format!(
                "{subject} names no file that OpenMaker opens beside “{from}”: it opens only \
                 plain file names in the same folder. Its tables are unavailable."
            )),
            SourcePaths::NoneFollowed => warnings.push(format!(
                "{subject} names no file OpenMaker opens: it opens a plain file name on the \
                 host “{from}” is on, or an fmnet path to a host and port, and does not \
                 follow folders, volumes or other forms. Its tables are unavailable."
            )),
        }
    }
    warnings
}

/// How many names a line quotes before saying how many more there are.
const NAMES_SHOWN: usize = 3;
/// The most characters of a name a line quotes; a longer one is cut with
/// an ellipsis. Above the length FileMaker gives its own names, so only a
/// malformed or hostile file's are cut.
const NAME_CHARS: usize = 100;

/// `name` as a line quotes it: at most [`NAME_CHARS`] characters.
fn shown_name(name: &str) -> std::borrow::Cow<'_, str> {
    match name.char_indices().nth(NAME_CHARS) {
        None => name.into(),
        Some(_) => {
            let cut = name
                .char_indices()
                .nth(NAME_CHARS - 1)
                .map_or(0, |(at, _)| at);
            format!("{}…", &name[..cut]).into()
        }
    }
}

/// The first names quoted, then how many more of `total`: “A”, “B” and
/// “C”, or “A”, “B”, “C”, and 2 more. `names` holds at least one.
fn quoted_names<S: AsRef<str>>(names: &[S], total: usize) -> String {
    let quoted: Vec<String> = names
        .iter()
        .take(NAMES_SHOWN)
        .map(|name| format!("“{}”", shown_name(name.as_ref())))
        .collect();
    match (quoted.as_slice(), total.max(quoted.len())) {
        ([one], 1) => one.clone(),
        (shown, total) if total > shown.len() => {
            format!("{}, and {} more", shown.join(", "), total - shown.len())
        }
        (shown, _) => match shown.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            Some((last, _)) => last.clone(),
            None => String::new(),
        },
    }
}

/// Data source `source`'s name, when it reads as one.
fn source_name(tree: &KeyTree<'_>, source: u64) -> Option<String> {
    match read_source_name(tree, source) {
        SourceName::Named(name) => Some(name),
        _ => None,
    }
}

/// What data source `source`'s name `10` holds.
enum SourceName {
    Named(String),
    /// A name that decodes to nothing but white space.
    Empty,
    /// A value that is not text OpenMaker decodes.
    Unreadable,
    /// No name.
    Absent,
}

fn read_source_name(tree: &KeyTree<'_>, source: u64) -> SourceName {
    match tree
        .at(&[DATA_SOURCES, 5, source, SOURCE_NAME])
        .map(Node::text)
    {
        None => SourceName::Absent,
        Some(Some(Ok(name))) if name.trim().is_empty() => SourceName::Empty,
        Some(Some(Ok(name))) => SourceName::Named(name),
        Some(_) => SourceName::Unreadable,
    }
}

/// What data source `source`'s file path list `82` holds for this client.
enum SourcePaths {
    /// The files it names by relative references, in order (at least one).
    Files(Vec<Located>),
    /// The sole path of a local source is read from a file variable.
    Variable(String),
    /// Paths, none of them a relative reference.
    NoneFollowed,
    /// A list of no paths.
    Empty,
    /// A list that does not decode (file-metadata.md 1.1).
    Malformed,
    /// No list.
    Absent,
}

impl SourcePaths {
    /// The files followed, none when there are none.
    fn paths(self) -> Vec<Located> {
        match self {
            SourcePaths::Files(files) => files,
            _ => Vec::new(),
        }
    }
}

/// What data source `source`'s file path list holds: [`SourcePaths`], its
/// files followed from a file on `endpoint` ([`followed_files`]). A list
/// stored in a form that does not reassemble is malformed, not absent.
fn source_files(tree: &KeyTree<'_>, source: u64, endpoint: Option<(&str, u16)>) -> SourcePaths {
    let Some(node) = tree.at(&[DATA_SOURCES, 5, source, FILE_REFERENCE]) else {
        return SourcePaths::Absent;
    };
    let Some(bytes) = node.assembled() else {
        return SourcePaths::Malformed;
    };
    match om_data::file_refs::decode(&bytes) {
        None => SourcePaths::Malformed,
        Some(paths) if paths.is_empty() => SourcePaths::Empty,
        Some(paths)
            if endpoint.is_none() && paths.len() == 1 && paths[0].global_variable().is_some() =>
        {
            SourcePaths::Variable(paths.into_iter().next().expect("one path").name)
        }
        Some(_) => match followed_files(&bytes, endpoint) {
            files if files.is_empty() => SourcePaths::NoneFollowed,
            files => SourcePaths::Files(files),
        },
    }
}

/// The files a data source's file reference `82` names that this client
/// opens, in its order: a `u32` count of alternatives, each a counted
/// scheme, location and file name (file-metadata.md 1.1). From a file on
/// this computer (`endpoint` `None`), a relative alternative: the `file`
/// scheme with an empty location and a plain name
/// ([`om_data::file_refs::FilePath::relative_name`]), beside it. From a file
/// on a host, the host and port the user opened (`endpoint`), a relative
/// alternative or an `fmnet` one naming that host and port
/// ([`om_data::file_refs::FilePath::same_host_name`]), on it. None when the
/// stream is malformed.
fn followed_files(bytes: &[u8], endpoint: Option<(&str, u16)>) -> Vec<Located> {
    om_data::file_refs::decode(bytes)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match endpoint {
            None => p.relative_name().map(|_| beside(&p.name)),
            Some((host, port)) if p.relative_name().is_some() => Some(Located {
                host: host.to_owned(),
                port,
                name: p.name,
            }),
            Some(_) => {
                let (host, port) = p.fmnet_endpoint()?;
                om_data::file_refs::is_plain_name(&p.name).then(|| Located {
                    host: host.to_owned(),
                    port,
                    name: p.name.clone(),
                })
            }
        })
        .collect()
}

/// The canonical wire name: add `.fmp12` only when absent, never strip
/// from a canonical name. `Root.fmp12.fmp12` stays distinct from `Root`.
fn hosted_name(name: &str) -> String {
    if name.to_lowercase().ends_with(".fmp12") {
        name.to_owned()
    } else {
        format!("{name}.fmp12")
    }
}

/// What the session's memory of refused Logins knows `target` by
/// (refusals.rs): its path beside a file on this computer, else its name on
/// the host.
fn refusal_key(auth: &Auth, target: &Target) -> crate::refusals::FileKey {
    match &target.path {
        Some(path) => crate::refusals::FileKey::local(path),
        None => crate::refusals::FileKey::hosted(&auth.host, auth.port, &target.name),
    }
}

/// [`hosted_name`] compared as FileMaker compares file names: without case.
fn hosted_key(name: &str) -> String {
    hosted_name(name).to_lowercase()
}

/// What tells a data source's list of paths on a host apart: their
/// [`hosted_key`]s in order, one per line. A list of one path has that
/// file's key, so it is the file's.
fn paths_key(paths: &[Located]) -> OsString {
    paths
        .iter()
        .map(|path| path.key().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n")
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_keep_subtrees_and_omit_missing_optional_catalogs() {
        let tables = om_hbam::OwnedNode::list(
            [3],
            vec![om_hbam::OwnedNode::leaf([1], b"table-definition")],
        );
        let functions = om_hbam::OwnedNode::list(
            [16],
            vec![om_hbam::OwnedNode::leaf([5], b"function-definition")],
        );
        let root = om_hbam::OwnedNode::list([], vec![tables.clone(), functions.clone()]);
        let reply = |slots| om_protocol::DownloadListReply {
            slots,
            status: 0,
            resume: om_protocol::Resume::complete(),
        };
        let keys: Vec<_> = RELATED_CATALOGS.into_iter().map(key_bytes).collect();
        let wire = root.to_wire().unwrap();
        let downloads = catalogs_from_list(reply(wire.clone()), &keys).unwrap();
        assert_eq!(downloads.len(), 2);
        assert_eq!(downloads[0].path, [vec![3]]);
        assert_eq!(downloads[0].slots, tables.to_wire().unwrap());
        assert_eq!(downloads[1].path, [vec![16]]);
        assert_eq!(downloads[1].slots, functions.to_wire().unwrap());
        assert!(catalogs_from_list(reply(wire[..wire.len() - 1].to_vec()), &keys).is_err());
        let missing_tables = om_hbam::OwnedNode::list([], vec![functions]);
        assert!(catalogs_from_list(reply(missing_tables.to_wire().unwrap()), &keys).is_err());
    }

    #[test]
    fn batches_open_files_in_parallel_and_answer_in_discovery_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Barrier;
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let started = Barrier::new(PARALLEL_LOADS);
        let files: Vec<_> = (0..PARALLEL_LOADS).map(|n| n.to_string()).collect();
        let results = load_batch(&files, &|file| {
            let index: usize = file.parse().unwrap();
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(count, Ordering::SeqCst);
            started.wait();
            // Finish in the opposite order to discovery.
            std::thread::sleep(Duration::from_millis(((PARALLEL_LOADS - index) * 3) as u64));
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(index)
        });
        assert_eq!(peak.load(Ordering::SeqCst), PARALLEL_LOADS);
        assert_eq!(
            results.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
            (0..PARALLEL_LOADS).collect::<Vec<_>>()
        );
    }

    #[test]
    fn one_failed_file_keeps_the_others_of_its_batch() {
        let files: Vec<String> = vec!["first".into(), "unavailable".into(), "last".into()];
        let results = load_batch(&files, &|file| {
            if file == "unavailable" {
                Err(DataError::new("unavailable fixture"))
            } else {
                Ok(file.to_owned())
            }
        });
        assert_eq!(results[0].as_ref().unwrap(), "first");
        assert!(results[1].is_err());
        assert_eq!(results[2].as_ref().unwrap(), "last");
    }

    fn reference(alternatives: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut bytes = (alternatives.len() as u32).to_be_bytes().to_vec();
        for (scheme, location, file) in alternatives {
            for text in [scheme, location, file] {
                bytes.extend(om_scsu::encode_counted(text));
            }
        }
        bytes
    }

    /// On a host, each path on the host and port it names, in the stored
    /// order: a relative one on the host and port of the file that names
    /// it, an `fmnet` one on its own, whichever host that is (#976). From a
    /// file on this computer, relative ones only. A folder on a host, a
    /// location not in FileMaker Pro's stored form, and other folders and
    /// volumes are never followed.
    #[test]
    fn each_path_is_followed_on_the_host_it_names_in_order() {
        let at = |authority: &str| format!("\u{E000}{authority}\u{E000}");
        let (this, this_port, other) = (
            at("this.invalid"),
            at("this.invalid:5003"),
            at("other.invalid:5100"),
        );
        let paths = reference(&[
            ("fmnet", &other, "Related"),
            ("fmnet", &this, "First"),
            ("file", "", "Related"),
            ("fmnet", &this_port, "Third"),
            ("file", "", "Fallback"),
        ]);
        let on = |host: &str, port, name: &str| Located {
            host: host.into(),
            port,
            name: name.into(),
        };
        assert_eq!(
            followed_files(&paths, Some(("this.invalid", 5003))),
            [
                on("other.invalid", 5100, "Related"),
                on("this.invalid", 5003, "First"),
                on("this.invalid", 5003, "Related"),
                on("this.invalid", 5003, "Third"),
                on("this.invalid", 5003, "Fallback"),
            ]
        );
        // From another port, the relative paths are there.
        assert_eq!(
            followed_files(&paths, Some(("this.invalid", 5200)))[2],
            on("this.invalid", 5200, "Related")
        );
        // From a file on this computer: no fmnet path at all.
        assert_eq!(
            followed_files(&paths, None),
            [beside("Related"), beside("Fallback")]
        );
        // One name on two hosts, or two ports, is two files; one host in
        // two cases is one.
        let related = |host: &str, port| on(host, port, "Related").key();
        assert_ne!(
            related("this.invalid", 5003),
            related("other.invalid", 5003)
        );
        assert_ne!(related("this.invalid", 5003), related("this.invalid", 5100));
        assert_eq!(related("this.invalid", 5003), related("THIS.invalid", 5003));
        assert_eq!(
            related("this.invalid", 5003),
            on("this.invalid", 5003, "related.FMP12").key()
        );
        for (scheme, location, file) in [
            ("fmnet", "/this.invalid:5003/", "Related"),
            (
                "fmnet",
                "\u{E000}this.invalid\u{E000}Folder\u{E000}",
                "Related",
            ),
            ("fmnet", other.as_str(), "../Related"),
            ("file", "", "../Other"),
            ("file", "/Volumes/", "Other"),
        ] {
            assert!(
                followed_files(
                    &reference(&[(scheme, location, file)]),
                    Some(("this.invalid", 5003))
                )
                .is_empty(),
                "{scheme} {location:?} {file}"
            );
        }
        assert_eq!(
            followed_files(&reference(&[("file", "", "Ünicode 日本語.fmp12")]), None),
            [beside("Ünicode 日本語.fmp12")]
        );
        let mut truncated = reference(&[("file", "", "Related")]);
        truncated.pop();
        assert!(followed_files(&truncated, Some(("this.invalid", 5003))).is_empty());
        let mut trailing = reference(&[("file", "", "Related")]);
        trailing.push(0);
        assert!(followed_files(&trailing, Some(("this.invalid", 5003))).is_empty());
    }

    #[test]
    fn discovery_and_warning_storage_stay_bounded() {
        let bindings: Vec<_> = (0..10_000)
            .map(|n| Binding {
                occurrence: n,
                occurrence_name: format!("Missing {n}"),
                target: Target {
                    name: format!("Missing {n}"),
                    key: hosted_key(&format!("Missing {n}")).into(),
                    path: None,
                    paths: vec![beside(&format!("Missing {n}"))],
                    from: "Root".into(),
                    source: None,
                },
                table: 1,
            })
            .collect();
        let (mut pending, mut seen, mut found, mut limited) =
            (Vec::new(), BTreeSet::new(), 0, false);
        discover(&bindings, &mut pending, &mut seen, &mut found, &mut limited);
        assert!(limited);
        assert_eq!(pending.len(), MAX_SOURCES);
        assert_eq!(seen.len(), MAX_SOURCES);
        let mut warnings = Warnings::default();
        warnings.extend((0..10_000).map(|n| format!("Problem {n}")));
        assert_eq!(warnings.lines.len(), MAX_WARNINGS - 1);
        let warnings = warnings.finish();
        assert_eq!(warnings.len(), MAX_WARNINGS);
        assert!(warnings.last().unwrap().contains("9981 more problems"));
    }

    #[test]
    fn a_related_file_on_another_host_names_its_host_and_port_when_it_differs() {
        let at = |host: &str, port: u16| Located {
            host: host.into(),
            port,
            name: "Peer.FMP12".into(),
        };
        let opened = Some(("host.invalid", 5003));
        // On the opened file's host and port, in any case: derived as before.
        assert_eq!(hosted_path(opened, &at("HOST.invalid", 5003), "Peer"), None);
        // Another host on the same port: no port.
        assert_eq!(
            hosted_path(opened, &at("other.invalid", 5003), "Peer").as_deref(),
            Some("fmnet:/other.invalid/Peer.fmp12")
        );
        // Another port of the same host, or of another: its port.
        assert_eq!(
            hosted_path(opened, &at("host.invalid", 5100), "Peer").as_deref(),
            Some("fmnet:/host.invalid:5100/Peer.fmp12")
        );
        assert_eq!(
            hosted_path(opened, &at("other.invalid", 5100), "Peer").as_deref(),
            Some("fmnet:/other.invalid:5100/Peer.fmp12")
        );
    }

    #[test]
    fn file_names_compare_without_case_with_the_extension_added_once() {
        assert_eq!(hosted_key("Alpha.FMP12"), hosted_key("alpha"));
        assert_ne!(hosted_key("Alpha copy"), hosted_key("Alpha"));
        assert_eq!(hosted_key("Root"), hosted_key("root.fmp12"));
        assert_ne!(hosted_key("Root.fmp12.fmp12"), hosted_key("Root"));
    }

    #[test]
    fn credentials_are_dropped_when_the_related_files_are_opened_and_on_close() {
        let connection = Connection {
            host: "127.0.0.1".into(),
            port: 5003,
            file: "Root.fmp12".into(),
            account: "Admin".into(),
            password: "secret".into(),
        };
        let config = NativeConfig {
            client_cert_pem: Vec::new(),
            client_key_pem: Vec::new(),
            additional_ca_pem: Vec::new(),
            callback_host: "127.0.0.1".into(),
            machine: om_protocol::MachineInfo::default(),
        };
        let mut files = ExternalFiles::for_connection(&connection, None);
        assert!(files.holds_credentials());
        files.close();
        assert!(!files.holds_credentials());

        // A file without data sources: nothing is opened, and nothing will
        // be after the schema is built.
        let mut files = ExternalFiles::for_connection(&connection, None);
        let tree = KeyTree::default();
        let catalog = Catalog::default();
        let mut schema = CalcSchema::new("Root", om_data::calc::Locale::en_us());
        files
            .hydrate(
                &config,
                &CloseRequests::default(),
                None,
                &mut schema,
                &catalog,
                &tree,
            )
            .unwrap();
        assert!(!files.holds_credentials());
        assert!(files.warnings().is_empty());
    }
}
