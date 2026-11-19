//! The script engine's session over a hosted file: the [`ScriptHost`] and
//! [`CalcHost`] a Perform Script on Server request runs against
//! (script-runtime.md 11.3, 11.5).
//!
//! Each request gets a fresh session: one window on the file's first
//! layout with all records, and none of the client's layout, found set,
//! record, global field values or variables. Records change as FileMaker Pro
//! changes them, through the server's own write path: a record is locked for
//! the session when a step first changes it, and committed with the file's
//! rules (auto-enter, stored calculations, validation: `om_data::rules`, the
//! desktop client's) and op70's machinery (`writes::commit_records`, which
//! keeps the record list and indexes current), so every client registered
//! on the file is told of the change as for any client's commit
//! (write-path.md 6). Inside Open Transaction the changes wait in an
//! overlay that reads, finds and found sets see, and Commit Transaction
//! writes them all or none. Calculations read the stored values, decoded
//! per field type in the file's formats ([`super::values`], every
//! repetition), related records through the file's relationships
//! ([`related`]), unstored calculations evaluated on the record and summary
//! fields over the found set ([`calculated`]); `Get` selectors that describe
//! the machine describe the server. Sort Records ([`sort`]) and finds with
//! stored requests ([`requests`]) work on the session's found set.
//!
//! The session has the requesting account's privileges: an account the
//! server does not know to have [Full Access] fails closed
//! ([`Privileges::Unknown`]), except in the steps of a script set to run
//! with full access privileges (script-runtime.md 9.1).
//!
//! `ExecuteSQL` and `ExecuteSQLe` query the file as the session sees it
//! ([`SessionSource`] under `om_data::host::sql`): the open record and an
//! open transaction's changes over the file's records, the session's
//! global values, `field = value` through the file's indexes; a session
//! without the privileges to read records gets error 9 for every query.
//!
//! Steps this host does not implement fail with FileMaker's error 3, as a
//! step without a handler does on FileMaker Server (script-runtime.md 10.2).

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{info, warn};
use om_calc::eval::{
    CalcHost, CalcLimits, Context, FieldId, FieldInfo, HostFunction, Storage, VarScope, Variables,
};
use om_calc::{CalcError, Ctx, Date, Locale, Time, TimeStyle, Timestamp, Value, ValueType};
use om_data::found::Findable;
use om_data::host::sql::FileSql;
use om_data::host::{CalcSchema, CalcSource, FileCalc, RecordValues, SessionInfo, Vars};
use om_data::rules::{self, Entry, RecordEnvironment, RecordPlan, WriteMode};
use om_data::sql::calc::Variant;
use om_data::sql::SqlError;
use om_data::summary::Summary;
use om_data::DataError;
use om_formula::{NameTable, Names};
use om_hbam::slot::uint_segment;
use om_hbam::{
    serial, Catalog, DataType, Field, FieldKind, FieldOptions, KeyTree, OwnedNode, RidList,
    SlotReader,
};
use om_protocol::write::{CreateRecord, GetSerial, KeyPath, LOCK_WRITE, SERIAL_INCREMENT};
use om_query::keys::TextKeys;
use om_scripts::runtime::Fields as FieldSteps;
use om_scripts::runtime::{
    Accounts, CommitOptions, Dialogs, ErrorCode, ErrorLogEntry, FieldRef, FileId, Files, FindKind,
    FoundSets, HostError, HostResult, InsertTarget, LayoutTarget, Mode, Navigation, Network,
    PauseEnd, Platform, RecordMove, Records, RelatedRecords, ScriptAccess, ScriptHost,
    TransactionOptions, WindowId, Windows,
};
use om_scripts::specs::SortSpec;
use om_scripts::{Script, ScriptCatalog, ScriptKind};

use super::find::{self, FindError, Request};
use super::{values, Control};
use crate::config::{Privileges, SCRIPT_CALC_LIMITS};
use crate::connection::Shared;
use crate::hosting::Hosted;
use crate::locks::LockUser;
use crate::modcount;
use crate::store::Store;
use crate::writes::{self, RecordWrite};

mod calculated;
mod containers;
mod design;
mod files;
mod get;
mod related;
mod requests;
mod sort;
mod transfer;
mod url;

/// Keys under a base table `T` (write-path.md 5, query-program.md 7.1;
/// the global values, calc-values.md 13).
const INDEXES: u8 = 0x01;
const RECORDS: u8 = 0x05;
const GLOBALS: u8 = 0x08;
const RECORD_IDS: u8 = 0x0d;
/// The value index under `[T, 01, F]`.
const VALUE_INDEX: u8 = 0x00;

/// The script catalog (scripts.md).
const SCRIPTS: u8 = 17;

/// Field option bit: global storage (schema-path.md 5.5).
const GLOBAL_STORAGE: u32 = 0x1_0000;

/// FileMaker errors this host reports.
const RECORD_IN_USE: ErrorCode = ErrorCode(301);
const FIELD_LOCKED: ErrorCode = ErrorCode(201);
const REPETITION_INVALID: ErrorCode = ErrorCode(111);
const TABLE_MISSING: ErrorCode = ErrorCode(106);
const NO_CRITERIA: ErrorCode = ErrorCode(400);
const NO_RECORDS_MATCH: ErrorCode = ErrorCode(401);
const NOT_UNIQUE: ErrorCode = ErrorCode(rules::error::NOT_UNIQUE);
const NOT_EXISTING: ErrorCode = ErrorCode(rules::error::NOT_EXISTING);
const NOT_IN_LIST: ErrorCode = ErrorCode(rules::error::NOT_IN_LIST);
/// Error 9, insufficient privileges, as a calculation's error.
const NO_PRIVILEGES: u16 = 9;

/// The step errors a session keeps for inspection, and the lines it writes
/// to the server's log; past these it only counts (`LogOrCountError`,
/// script-runtime.md 4.9).
const ERROR_LOG_KEPT: usize = 64;
const ERROR_LOG_LINES: u64 = 100;
/// How long a pause sleeps between checks of the script's abort flag
/// (`SMScriptRuntime::OnIdle` sleeps at most 100 ms at a time, 6.2).
const PAUSE_SLICE: Duration = Duration::from_millis(100);
/// How many records a scan or sort inside one step visits between checks
/// of the script's abort flag and time limit.
const ABORT_CHECK: usize = 256;
/// How many finders of one find request a query runs at a time. A
/// criterion becomes a finder per word, so a request can have thousands;
/// between batches the script's abort flag and time limit are checked, and
/// the file's store is not held for the whole find.
const FINDER_BATCH: usize = 64;

/// The record a window's calculations are evaluated on when it has no
/// current record (in Find mode, with an empty found set): field
/// references read the current find request's criteria or empty values
/// there and global fields their values, as FileMaker Server's engine
/// reads them (script oracle s18-find, script-runtime.md 13.21), where a
/// context without a record ends a field reference with 1224
/// (calc-values.md 13). No record has this id.
const NO_RECORD: u64 = u64::MAX;

/// The one window of a session.
const WINDOW: WindowId = 1;
/// The session's only file.
const FILE: FileId = 0;

/// `Get` selectors this host answers (catalogs/get-selectors.csv).
mod selector {
    pub const APPLICATION_VERSION: u8 = 1;
    pub const CURRENT_DATE: u8 = 2;
    pub const FILE_NAME: u8 = 5;
    pub const FILE_SIZE: u8 = 6;
    pub const FOUND_COUNT: u8 = 7;
    pub const HOST_NAME: u8 = 8;
    pub const LAYOUT_COUNT: u8 = 9;
    pub const LAYOUT_NAME: u8 = 10;
    pub const LAYOUT_NUMBER: u8 = 11;
    pub const SYSTEM_LANGUAGE: u8 = 12;
    pub const WINDOW_MODE: u8 = 13;
    pub const SYSTEM_PLATFORM: u8 = 16;
    pub const TOTAL_RECORD_COUNT: u8 = 19;
    pub const RECORD_NUMBER: u8 = 20;
    pub const REQUEST_COUNT: u8 = 22;
    pub const SORT_STATE: u8 = 27;
    pub const CURRENT_TIME: u8 = 29;
    pub const USER_NAME: u8 = 31;
    pub const ACCOUNT_NAME: u8 = 32;
    pub const RECORD_ID: u8 = 37;
    pub const RECORD_MODIFICATION_COUNT: u8 = 38;
    pub const FILE_PATH: u8 = 40;
    pub const APPLICATION_LANGUAGE: u8 = 59;
    pub const CURRENT_HOST_TIMESTAMP: u8 = 60;
    pub const LAYOUT_TABLE_NAME: u8 = 61;
    pub const CURRENT_TIMESTAMP: u8 = 63;
    pub const RECORD_OPEN_COUNT: u8 = 78;
    pub const RECORD_OPEN_STATE: u8 = 79;
    pub const UUID: u8 = 101;
    pub const PERSISTENT_ID: u8 = 102;
    pub const CURRENT_TIME_UTC_MILLISECONDS: u8 = 104;
    pub const DEVICE: u8 = 105;
    pub const MODIFIED_FIELDS: u8 = 110;
    pub const APPLICATION_ARCHITECTURE: u8 = 115;
    pub const FILE_LOCALE_ELEMENTS: u8 = 127;
    pub const CURRENT_TIME_UTC_MICROSECONDS: u8 = 133;
}

/// Microseconds from 1/1/0001 to the Unix epoch (FileMaker counts its UTC
/// clock from the former).
const EPOCH_MICROS: i128 = 62_135_596_800_000_000;

/// A layout as the session sees it.
#[derive(Clone, Debug)]
struct LayoutEntry {
    id: u64,
    name: String,
    /// The table occurrence it shows, when the occurrence exists.
    occurrence: Option<u64>,
}

/// The file's schema, read when the session starts and again after each
/// change ([`ServerHost::refresh_schema`]).
struct Schema {
    /// The store's schema generation it was read at.
    generation: u64,
    catalog: Catalog,
    names: NameTable,
    /// Table occurrence -> base table.
    occurrences: HashMap<u64, u64>,
    layouts: Vec<LayoutEntry>,
    scripts: Vec<Arc<Script>>,
    /// The scripts set to run with full access privileges.
    full_access: BTreeSet<u64>,
    /// The catalogs record rules read, as a slot list ([`rule_catalogs`]),
    /// and the file's calculation schema from them; `None` when they could
    /// not be read (every write is then refused).
    rules: Option<(Vec<u8>, Arc<CalcSchema>)>,
    /// The definitions of summary fields, by (table, field).
    summaries: HashMap<(u64, u64), Summary>,
    /// The schema with layouts and scripts for the Design functions, read
    /// when a calculation first asks ([`design`]).
    design: std::sync::OnceLock<Option<Arc<CalcSchema>>>,
}

impl Schema {
    fn load(store: &Store, file: &str) -> Schema {
        let catalog = store.catalog().clone();
        let rules = rule_catalogs(store, &catalog).and_then(|wire| {
            let tree = KeyTree::from_slots(SlotReader::new(&wire)).ok()?;
            let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
            let mut calc = rules::schema::calc_schema(&tree, &catalog, stem);
            calc.locale = store.locale();
            Some((wire, Arc::new(calc)))
        });
        let mut names = NameTable::default();
        let mut occurrences = HashMap::new();
        for occurrence in &catalog.table_occurrences {
            if let Some(table) = occurrence.base_table() {
                names.add_occurrence(occurrence.id, &occurrence.name, table);
                occurrences.insert(occurrence.id, table);
            }
        }
        for table in &catalog.tables {
            for field in &table.fields {
                names.add_field(table.id, field.id, &field.name);
                names.set_summary_field(table.id, field.id, field.kind == FieldKind::Summary);
            }
        }
        let layouts = catalog
            .layouts
            .iter()
            .map(|layout| LayoutEntry {
                id: layout.id,
                name: layout.name.clone(),
                occurrence: layout
                    .layout_def
                    .as_deref()
                    .and_then(|def| om_layout::LayoutDef::decode(def).ok())
                    .and_then(|def| om_layout::occurrence_id(def.table_alias_key))
                    .filter(|o| occurrences.contains_key(o)),
            })
            .collect();
        let scripts = store
            .node(&[&[SCRIPTS]])
            .map(|node| scripts_of(node, file))
            .unwrap_or_default();
        let full_access = scripts
            .iter()
            .filter(|s| s.runs_with_full_access())
            .map(|s| s.id)
            .collect();
        Schema {
            generation: store.schema_generation(),
            catalog,
            names,
            occurrences,
            layouts,
            scripts,
            full_access,
            rules,
            summaries: calculated::summaries(store),
            design: std::sync::OnceLock::new(),
        }
    }

    /// The record rules of `table` (`om_data::rules`): an error when the
    /// server cannot enforce one of them.
    fn plan(&self, table: u64) -> Result<RecordPlan, DataError> {
        let (wire, calc) = self
            .rules
            .as_ref()
            .ok_or_else(|| DataError::new("The file's field catalogs could not be read."))?;
        let tree = KeyTree::from_slots(SlotReader::new(wire))
            .map_err(|e| DataError::new(format!("The file's field catalogs: {e}")))?;
        let definition = self
            .catalog
            .table(table)
            .ok_or_else(|| DataError::with_code(106, "The table is missing."))?;
        RecordPlan::from_file_schema(definition, &tree, calc)
    }

    fn table_of(&self, occurrence: u64) -> Option<u64> {
        self.occurrences.get(&occurrence).copied()
    }

    /// The definition of a field of the table `occurrence` shows.
    fn field(&self, occurrence: u64, field: u64) -> Option<(u64, &Field)> {
        let table = self.table_of(occurrence)?;
        Some((table, self.catalog.table(table)?.field(field)?))
    }
}

/// The catalogs the record rules and ExecuteSQL read, as one slot list:
/// the schema `[3]` (occurrences, relationships), custom functions `[16]`,
/// value lists `[21]` and each table's field catalog `[T, 3]`.
fn rule_catalogs(store: &Store, catalog: &Catalog) -> Option<Vec<u8>> {
    let mut children: Vec<OwnedNode> = [3u8, 16, 0x21]
        .iter()
        .filter_map(|key| store.node(&[&[*key]]).cloned())
        .collect();
    for table in &catalog.tables {
        let t = uint_segment(table.id);
        if let Some(fields) = store.node(&[&t, &[3]]) {
            children.push(OwnedNode::list(t, vec![fields.clone()]));
        }
    }
    OwnedNode::list(Vec::new(), children).to_wire()
}

/// The scripts of a script catalog node `[17]`.
fn scripts_of(node: &OwnedNode, file: &str) -> Vec<Arc<Script>> {
    let decoded = OwnedNode::list(Vec::new(), vec![node.clone()])
        .to_wire()
        .ok_or_else(|| "a key too long for a slot".to_string())
        .and_then(|wire| {
            let tree = KeyTree::from_slots(SlotReader::new(&wire)).map_err(|e| e.to_string())?;
            ScriptCatalog::decode(&tree).map_err(|e| e.to_string())
        });
    match decoded {
        Ok(catalog) => catalog.scripts.into_iter().map(Arc::new).collect(),
        Err(e) => {
            warn!("{file:?}: reading the script catalog: {e}");
            Vec::new()
        }
    }
}

/// The session's window: its layout, found set, current record and mode.
#[derive(Clone, Debug)]
struct Window {
    /// Index into the schema's layouts.
    layout: usize,
    occurrence: u64,
    table: u64,
    /// Record ids in found-set order.
    found: Vec<u64>,
    current: usize,
    mode: Mode,
    /// The current record when Find mode was entered, which a find
    /// without criteria returns to.
    browse_current: usize,
    /// The sort order of the found set (Sort Records), until the found
    /// set is replaced.
    sort: Option<SortSpec>,
}

impl Window {
    fn record(&self) -> Option<u64> {
        self.found.get(self.current).copied()
    }
}

/// A record being edited: locked for the session, its contents changed
/// here until it is committed (FileMaker's open record).
#[derive(Clone, Debug)]
struct OpenRecord {
    table: u64,
    id: u64,
    /// The whole record as it will be written.
    node: OwnedNode,
    /// Created by New Record and never committed.
    new: bool,
    /// The cells steps set (Set Field, an imported row); the rules' own
    /// values are not among them.
    changed: BTreeSet<u64>,
    /// Set when the rules already ran on the record (an imported row,
    /// with the import's own entry options): a commit writes what they
    /// left and applies its rules only to the cells changed since.
    ruled: Option<Box<Ruled>>,
    /// The serial numbers generated on creation that a new record took
    /// when New Record created it, by field: in `node` from then on, and
    /// entered again by its rules at the commit.
    serials: BTreeMap<u64, String>,
    /// The window's layout number and the current privilege set when a
    /// step last opened or changed the record ([`ServerHost::step_context`]):
    /// what its formulas read at the commit, which may come after the
    /// script went to another layout or left a step running with full
    /// access (Commit Transaction, #187).
    /// Boxed: an open record is held in a transaction's [`Change`].
    context: Box<rules::WriteContext>,
}

/// The record rules' state of an [`OpenRecord`] they already ran on.
#[derive(Clone, Debug, Default)]
struct Ruled {
    /// Every cell the rules wrote (auto-entered and computed values with
    /// the entered ones): what a rebase on the file keeps.
    written: BTreeSet<u64>,
    /// The record's text as the rules left it, kept when a cell first
    /// changes after them: the base the next rules run starts from.
    base: Option<BTreeMap<u64, String>>,
    /// The cells changed since the rules ran.
    since: BTreeSet<u64>,
}

impl OpenRecord {
    /// Before a step changes a cell: keep the record as the rules left it.
    fn before_change(&mut self) {
        if let Some(ruled) = &mut self.ruled {
            if ruled.base.is_none() {
                ruled.base = Some(texts(&self.node));
            }
        }
    }

    /// A step changed `cell`.
    fn changed_cell(&mut self, cell: u64) {
        self.changed.insert(cell);
        if let Some(ruled) = &mut self.ruled {
            ruled.since.insert(cell);
        }
    }
}

/// What an open transaction holds for one record.
#[derive(Clone, Debug)]
enum Change {
    /// The record as it will be written: every edit since Open Transaction
    /// merged, the fields they changed, created in the transaction or not.
    Write(OpenRecord),
    /// A record of the file deleted in the transaction.
    Delete,
}

/// Open Transaction: its options and the changes Commit Transaction writes,
/// by (table, record). Reads, finds and found sets go through it; the
/// records it holds stay locked until the transaction ends.
#[derive(Debug)]
struct Transaction {
    options: TransactionOptions,
    changes: BTreeMap<(u64, u64), Change>,
    /// The text index keys the changed records hold in fields whose values
    /// must be unique or existing, by (table, field), built when a write
    /// first checks one and kept current by every change: checking a value
    /// against the transaction's records costs a lookup, not a pass over
    /// them (an import stages every row here).
    values: std::cell::RefCell<HashMap<(u64, u64), FieldValues>>,
}

/// The records of a transaction holding each text index key in one field.
#[derive(Debug)]
struct FieldValues {
    /// The field's cells (its repetitions).
    cells: Vec<u64>,
    /// How the field's index keys text.
    keys: TextKeys,
    holders: HashMap<Vec<u8>, BTreeSet<u64>>,
}

impl FieldValues {
    /// The keys `node` holds in the field.
    fn keys_of(&self, node: &OwnedNode) -> BTreeSet<Vec<u8>> {
        self.cells
            .iter()
            .filter_map(|cell| node.child(&uint_segment(*cell)).and_then(values::cell_text))
            .filter(|text| !text.is_empty())
            .filter_map(|text| self.keys.key(&text))
            .collect()
    }

    fn add(&mut self, id: u64, change: &Change) {
        if let Change::Write(record) = change {
            for key in self.keys_of(&record.node) {
                self.holders.entry(key).or_default().insert(id);
            }
        }
    }

    fn take(&mut self, id: u64, change: &Change) {
        if let Change::Write(record) = change {
            for key in self.keys_of(&record.node) {
                if let Some(ids) = self.holders.get_mut(&key) {
                    ids.remove(&id);
                    if ids.is_empty() {
                        self.holders.remove(&key);
                    }
                }
            }
        }
    }
}

impl Transaction {
    fn new(options: TransactionOptions) -> Self {
        Transaction {
            options,
            changes: BTreeMap::new(),
            values: Default::default(),
        }
    }

    /// Set the change of record `key`, returning the one it replaces.
    fn insert(&mut self, key: (u64, u64), change: Change) -> Option<Change> {
        let old = self.changes.remove(&key);
        for ((table, _), values) in self.values.get_mut().iter_mut() {
            if *table == key.0 {
                if let Some(old) = &old {
                    values.take(key.1, old);
                }
                values.add(key.1, &change);
            }
        }
        self.changes.insert(key, change);
        old
    }

    fn remove(&mut self, key: &(u64, u64)) -> Option<Change> {
        let old = self.changes.remove(key)?;
        for ((table, _), values) in self.values.get_mut().iter_mut() {
            if *table == key.0 {
                values.take(key.1, &old);
            }
        }
        Some(old)
    }

    /// Change every held record in place (a rebase on the file); the value
    /// index is built again when next needed.
    fn rebase_each(&mut self, mut rebase: impl FnMut(&mut OpenRecord)) {
        for change in self.changes.values_mut() {
            if let Change::Write(record) = change {
                rebase(record);
            }
        }
        self.values.get_mut().clear();
    }

    /// The records of `table` the transaction changed that hold text index
    /// key `key` (keyed as `keys`) in one of `cells`, the repetitions of
    /// `field`.
    fn holders(
        &self,
        table: u64,
        field: u64,
        cells: &[u64],
        (keys, key): (TextKeys, &[u8]),
    ) -> BTreeSet<u64> {
        let mut values = self.values.borrow_mut();
        let field_values = values.entry((table, field)).or_insert_with(|| {
            let mut built = FieldValues {
                cells: cells.to_vec(),
                keys,
                holders: HashMap::new(),
            };
            for ((_, id), change) in self.changes.range((table, 0)..=(table, u64::MAX)) {
                built.add(*id, change);
            }
            built
        });
        field_values.holders.get(key).cloned().unwrap_or_default()
    }
}

/// The step errors of a session, as the server's event log has them
/// (script-runtime.md 4.9): the last [`ERROR_LOG_KEPT`] kept, the first
/// [`ERROR_LOG_LINES`] logged, a run of the same error at the same step
/// logged once with its count.
#[derive(Debug, Default)]
pub(crate) struct ErrorLog {
    recent: VecDeque<ErrorLogEntry>,
    total: u64,
    lines: u64,
    /// The last error's place and how often it repeated since it was
    /// logged.
    repeated: Option<(String, u64)>,
}

impl ErrorLog {
    fn push(&mut self, file: &str, entry: &ErrorLogEntry) {
        self.total += 1;
        if self.recent.len() == ERROR_LOG_KEPT {
            self.recent.pop_front();
        }
        self.recent.push_back(entry.clone());
        let line = format!(
            "{file:?}: script \"{}\" - step \"{}\" (line {}): {}",
            entry.script, entry.step_name, entry.line, entry.code
        );
        match &mut self.repeated {
            Some((last, count)) if *last == line => *count += 1,
            _ => {
                self.flush();
                if self.lines < ERROR_LOG_LINES {
                    self.lines += 1;
                    info!("{line}");
                }
                self.repeated = Some((line, 0));
            }
        }
    }

    /// Log how often the last error repeated.
    fn flush(&mut self) {
        if let Some((line, count)) = self.repeated.take() {
            if count > 0 && self.lines < ERROR_LOG_LINES {
                self.lines += 1;
                info!("{line} (repeated {count} more times)");
            }
        }
    }

    /// The step errors kept, oldest first.
    #[cfg(test)]
    pub fn entries(&self) -> impl Iterator<Item = &ErrorLogEntry> {
        self.recent.iter()
    }

    /// How many step errors there were.
    pub fn total(&self) -> u64 {
        self.total
    }
}

/// `[T, 05, R]`.
fn record_path(table: u64, id: u64) -> KeyPath {
    vec![uint_segment(table), vec![RECORDS], serial::encode(id)]
}

fn segments(path: &KeyPath) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

/// The cell of repetition `repetition` of `field` in a record: the field's
/// own key for the first, `F * 256 + n` for repetitions 2 to 255
/// (execute-sql.md, reading `.fmp12` files); `None` beyond, whose storage
/// is not specified.
fn cell_id(field: u64, repetition: u32) -> Option<u64> {
    match repetition {
        0 | 1 => Some(field),
        2..=255 => field.checked_mul(256).map(|f| f + u64::from(repetition)),
        _ => None,
    }
}

/// A server script session.
pub(crate) struct ServerHost {
    shared: Arc<Shared>,
    hosted: Arc<Hosted>,
    /// The session's lock user; its connection id is the session's own, so
    /// its changes notify every client, the requester included.
    user: LockUser,
    /// What other users see as the holder of the session's locks.
    holder: String,
    account: String,
    locale: Locale,
    today: Date,
    schema: Schema,
    window: Option<Window>,
    record: Option<OpenRecord>,
    /// Open Transaction and the changes since.
    transaction: Option<Transaction>,
    /// Global field values of this session (no client's are passed).
    globals: HashMap<(u64, u64, u32), Value>,
    locals: Variables,
    global_vars: Variables,
    /// The requests of Find mode.
    requests: Vec<Request>,
    last_find: Vec<Request>,
    /// How many requests the last Perform Find ran with, found or not, or
    /// Find mode had when the window left it: `Get ( RequestCount )`
    /// outside Find mode (script oracle s18-find, s31, s33).
    request_count: usize,
    /// The field with the focus ([`get`]).
    focus: Option<get::Focus>,
    /// Find mode's New Request made the current request, which counts as
    /// an open record (`Get ( RecordOpenState )` 1, s31, s33).
    request_open: bool,
    /// Step errors, as the server's event log has them (script-runtime.md
    /// 4.9).
    pub log: ErrorLog,
    /// What the account may do.
    privileges: Privileges,
    /// A step of a script set to run with full access privileges is
    /// executing (script-runtime.md 9.1).
    elevated: bool,
    /// The script's abort flag and time limit.
    control: Control,
    /// What one calculation of the script may build.
    calc_limits: CalcLimits,
    /// The data files the session has open ([`files`]), closed when it
    /// ends.
    data_files: files::DataFiles,
    /// Each table's record rules, read when the session first writes to it.
    plans: HashMap<u64, Result<Arc<RecordPlan>, DataError>>,
    /// How deep unstored calculations and summaries are being read inside
    /// one another ([`calculated`]).
    reading: Cell<u32>,
    /// Container data steps set that no commit stored yet ([`containers`]).
    pending: containers::Pending,
}

impl ServerHost {
    /// A session of `account` with `privileges` on `hosted`, with lock user
    /// connection `session` (an id no client connection uses).
    pub fn new(
        shared: Arc<Shared>,
        hosted: Arc<Hosted>,
        session: u32,
        account: &str,
        privileges: Privileges,
    ) -> Self {
        // The file's formats (file-metadata.md 2): FileMaker Server
        // evaluates with the file's locale record.
        let (schema, locale) = {
            let store = hosted.read_store();
            (Schema::load(&store, &hosted.name), store.locale())
        };
        let mut host = ServerHost {
            holder: format!("{} (script engine, {account})", shared.identity.name),
            shared,
            hosted,
            user: LockUser {
                connection: session,
                txn: 1,
            },
            account: account.to_string(),
            locale,
            today: today(),
            schema,
            window: None,
            record: None,
            transaction: None,
            globals: HashMap::new(),
            locals: Variables::new(),
            global_vars: Variables::new(),
            requests: Vec::new(),
            last_find: Vec::new(),
            request_count: 0,
            focus: None,
            request_open: false,
            log: ErrorLog::default(),
            privileges,
            elevated: false,
            control: Control::unlimited(),
            calc_limits: SCRIPT_CALC_LIMITS,
            data_files: files::DataFiles::default(),
            plans: HashMap::new(),
            reading: Cell::new(0),
            pending: containers::Pending::default(),
        };
        // The file's "switch to layout" layout, else its first layout,
        // with all records (script-runtime.md 9.2).
        let opening = {
            let store = host.hosted.read_store();
            design::opening_layout(&store)
        };
        if let Some(index) = opening
            .and_then(|id| {
                host.schema
                    .layouts
                    .iter()
                    .position(|l| l.id == id && l.occurrence.is_some())
            })
            .or_else(|| {
                host.schema
                    .layouts
                    .iter()
                    .position(|l| l.occurrence.is_some())
            })
        {
            host.window = host.window_on(index);
        }
        host
    }

    #[cfg(test)]
    pub fn calc_context_for_test(&mut self) -> Context {
        ScriptHost::calc_context(self)
    }

    /// Whether the session has a window: FileMaker's error 112 otherwise
    /// (`SMScriptRuntime::ExecuteScript`, 11.3).
    pub fn has_window(&self) -> bool {
        self.window.is_some()
    }

    /// The script's abort flag and time limit.
    pub fn set_control(&mut self, control: Control) {
        self.control = control;
    }

    /// What one calculation of the script may build.
    pub fn set_calc_limits(&mut self, limits: CalcLimits) {
        self.calc_limits = limits;
    }

    /// End the session: revert an unfinished transaction, commit the open
    /// record as closing the window does (revert it when the script was
    /// aborted or the commit fails) and release every lock.
    pub fn close(&mut self) {
        self.refresh_schema();
        if self.transaction.is_some() {
            let _ = Records::revert_transaction(self);
        }
        if self.control.aborted().is_some() {
            self.drop_open();
        } else if let Err(e) = self.commit_open(Entry::script(false, false)) {
            warn!(
                "{:?}: the script's last record was not committed: {e:?}",
                self.hosted.name
            );
            self.drop_open();
        }
        self.release_locks();
        self.hosted.save();
        self.data_files.close_all();
        self.remove_temporary_folder();
        self.log.flush();
        if self.log.total() > ERROR_LOG_LINES {
            info!(
                "{:?}: the script had {} step errors",
                self.hosted.name,
                self.log.total()
            );
        }
    }

    /// Release every lock of the session (also those of a transaction or
    /// record it did not end).
    fn release_locks(&self) {
        self.hosted
            .lock_table()
            .release_connection(self.user.connection);
    }

    /// Whether the session may read and write records: an account with
    /// [Full Access], or any account in a step of a script set to run with
    /// full access privileges.
    fn privileged(&self) -> bool {
        self.privileges == Privileges::FullAccess || self.elevated
    }

    /// FileMaker's error 9 unless [`Self::privileged`]: the account's
    /// privilege set is not decoded, so nothing is allowed (fail closed).
    fn require_privileges(&self) -> HostResult {
        if self.privileged() {
            Ok(())
        } else {
            Err(ErrorCode::INSUFFICIENT_PRIVILEGES.into())
        }
    }

    /// The record rules of `table`; a table whose rules the server cannot
    /// enforce refuses writes (3, or the rule's own error).
    fn plan(&mut self, table: u64) -> HostResult<Arc<RecordPlan>> {
        let plan = self.plans.entry(table).or_insert_with(|| {
            let plan = self.schema.plan(table).map(Arc::new);
            if let Err(e) = &plan {
                warn!(
                    "{:?}: script writes to table {table:#x} are refused: {e}",
                    self.hosted.name
                );
            }
            plan
        });
        match plan {
            Ok(plan) => Ok(plan.clone()),
            Err(e) => Err(rule_error(e)),
        }
    }

    /// Read the schema again when it changed since the session read it
    /// (`Store::schema_generation`: Manage Database saved catalogs, or
    /// op73 did its field work): the record rules, the calculation and SQL
    /// schema, summaries, layouts and scripts are then the file's current
    /// ones. The window stays on its layout. The records the session holds
    /// (its open record, its transaction's) keep the cells the script
    /// changed over what the file now stores: op73 may have recomputed
    /// their stored calculations or dropped a deleted field's values.
    pub fn refresh_schema(&mut self) {
        let (schema, locale) = {
            let store = self.hosted.read_store();
            if store.schema_generation() == self.schema.generation {
                return;
            }
            (Schema::load(&store, &self.hosted.name), store.locale())
        };
        info!(
            "{:?}: script session reads the file's changed schema",
            self.hosted.name
        );
        let layout = self
            .window
            .as_ref()
            .and_then(|w| self.schema.layouts.get(w.layout))
            .map(|l| l.id);
        self.schema = schema;
        self.locale = locale;
        self.plans.clear();
        self.place_window(layout);
        if let Some(mut record) = self.record.take() {
            self.rebase(&mut record);
            self.record = Some(record);
        }
        if let Some(mut transaction) = self.transaction.take() {
            transaction.rebase_each(|record| self.rebase(record));
            self.transaction = Some(transaction);
        }
    }

    /// Put the window back on layout `id` in the schema just read. When the
    /// layout is gone, or no longer shows the window's table, the window
    /// moves to the first layout of its table (keeping its found set), else
    /// to the file's first layout with a table occurrence, with all of that
    /// table's records; without one it shows no layout (OpenMaker's choice:
    /// what FileMaker Server does when a running script's layout is deleted
    /// is not observed).
    fn place_window(&mut self, id: Option<u64>) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let table = window.table;
        let shows_table = |l: &LayoutEntry| {
            l.occurrence
                .is_some_and(|o| self.schema.table_of(o) == Some(table))
        };
        let layouts = &self.schema.layouts;
        let same = id
            .and_then(|id| layouts.iter().position(|l| l.id == id))
            .filter(|&i| shows_table(&layouts[i]));
        let index = same.or_else(|| layouts.iter().position(shows_table));
        match index {
            Some(index) => {
                let occurrence = layouts[index].occurrence;
                let window = self.window.as_mut().expect("checked");
                window.layout = index;
                window.occurrence = occurrence.expect("shows the table");
            }
            None => {
                let other = layouts.iter().position(|l| l.occurrence.is_some());
                match other.and_then(|index| self.window_on(index)) {
                    Some(window) => self.window = Some(window),
                    None => {
                        let window = self.window.as_mut().expect("checked");
                        window.layout = usize::MAX;
                    }
                }
            }
        }
    }

    /// A held record as the file stores it now with the cells the script
    /// changed over it (and, for a record the rules already ran on, the
    /// cells they wrote).
    fn rebase(&self, record: &mut OpenRecord) {
        if record.new {
            return;
        }
        let Some(mut current) = self.committed_node(record.table, record.id) else {
            return;
        };
        let mut kept = record.changed.clone();
        if let Some(ruled) = &record.ruled {
            kept.extend(&ruled.written);
        }
        for field in &kept {
            let key = uint_segment(*field);
            match record.node.child(&key) {
                Some(cell) => current.set_child(cell.clone()),
                None => {
                    current.remove(&[&key]);
                }
            }
        }
        if let Some(base) = record.ruled.as_mut().and_then(|r| r.base.as_mut()) {
            let mut rebased = texts(&current);
            for cell in &kept {
                match base.get(cell) {
                    Some(text) => rebased.insert(*cell, text.clone()),
                    None => rebased.remove(cell),
                };
            }
            *base = rebased;
        }
        record.node = current;
    }

    fn cx(&self) -> Ctx<'_> {
        Ctx::new(&self.locale, self.today)
    }

    /// A window on layout `index`, with every record of its table.
    fn window_on(&self, index: usize) -> Option<Window> {
        let occurrence = self.schema.layouts.get(index)?.occurrence?;
        let table = self.schema.table_of(occurrence)?;
        Some(Window {
            layout: index,
            occurrence,
            table,
            found: self.all_records(table),
            current: 0,
            mode: Mode::Browse,
            browse_current: 0,
            sort: None,
        })
    }

    fn window(&self) -> HostResult<&Window> {
        self.window.as_ref().ok_or(ErrorCode::WINDOW_MISSING.into())
    }

    fn window_mut(&mut self) -> HostResult<&mut Window> {
        self.window.as_mut().ok_or(ErrorCode::WINDOW_MISSING.into())
    }

    /// Every record of `table` in creation order (`[T, 0d]`), as the
    /// session sees them: with the records its transaction created and
    /// without those it deleted.
    fn all_records(&self, table: u64) -> Vec<u64> {
        let open_new = self
            .record
            .as_ref()
            .is_some_and(|r| r.new && r.table == table);
        if !open_new && self.pending_in(table).next().is_none() {
            return self.committed_records(table);
        }
        let mut ids: BTreeSet<u64> = self.committed_records(table).into_iter().collect();
        for ((_, id), change) in self.pending_in(table) {
            match change {
                Change::Write(record) if record.new => {
                    ids.insert(*id);
                }
                Change::Write(_) => {}
                Change::Delete => {
                    ids.remove(id);
                }
            }
        }
        if let Some(open) = self.record.as_ref().filter(|r| r.new && r.table == table) {
            ids.insert(open.id);
        }
        ids.into_iter().collect()
    }

    /// Every record of `table` in the file.
    fn committed_records(&self, table: u64) -> Vec<u64> {
        let store = self.hosted.read_store();
        let t = uint_segment(table);
        match store
            .node(&[&t, &[RECORD_IDS]])
            .and_then(|n| RidList::from_owned(n).ok())
        {
            Some(ids) => ids.iter().collect(),
            None => store
                .node(&[&t, &[RECORDS]])
                .map(|n| {
                    n.children
                        .iter()
                        .filter_map(|r| serial::decode(&r.segment))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The transaction's changes to records of `table`.
    fn pending_in(&self, table: u64) -> impl Iterator<Item = (&(u64, u64), &Change)> {
        self.transaction
            .iter()
            .flat_map(move |t| t.changes.range((table, 0)..=(table, u64::MAX)))
    }

    /// The transaction's change to a record, if any.
    fn pending(&self, table: u64, id: u64) -> Option<&Change> {
        self.transaction.as_ref()?.changes.get(&(table, id))
    }

    /// A record as the session sees it: the open record, else the
    /// transaction's version, else the file's; `None` when it does not
    /// exist (or the transaction deleted it).
    fn record_node(&self, table: u64, id: u64) -> Option<OwnedNode> {
        self.with_record(table, id, |node| node.cloned())
    }

    fn cell(&self, table: u64, id: u64, field: u64) -> Option<OwnedNode> {
        let field = uint_segment(field);
        self.with_record(table, id, |node| node?.child(&field).cloned())
    }

    fn with_record<T>(&self, table: u64, id: u64, read: impl FnOnce(Option<&OwnedNode>) -> T) -> T {
        if let Some(open) = self
            .record
            .as_ref()
            .filter(|r| r.table == table && r.id == id)
        {
            return read(Some(&open.node));
        }
        match self.pending(table, id) {
            Some(Change::Write(record)) => return read(Some(&record.node)),
            Some(Change::Delete) => return read(None),
            None => {}
        }
        let path = record_path(table, id);
        read(self.hosted.read_store().node(&segments(&path)))
    }

    /// The record as the file holds it.
    fn committed_node(&self, table: u64, id: u64) -> Option<OwnedNode> {
        let path = record_path(table, id);
        self.hosted.read_store().node(&segments(&path)).cloned()
    }

    fn lock_record(&self, table: u64, id: u64) -> HostResult {
        self.hosted
            .lock_table()
            .lock(&record_path(table, id), self.user, LOCK_WRITE, &self.holder)
            .map_err(|conflict| {
                info!(
                    "{:?}: script session refused record {id}: in use by {}",
                    self.hosted.name, conflict.users
                );
                HostError::Error(RECORD_IN_USE)
            })
    }

    fn unlock_record(&self, table: u64, id: u64) {
        self.hosted
            .lock_table()
            .release_path(&record_path(table, id), self.user);
    }

    /// The current record, opened for editing: locked for the session.
    /// Its table's rules must be ones the server enforces. Every call is an
    /// edit by the step that makes it, checked against that step's
    /// privileges, also when an earlier step (a caller set to run with full
    /// access privileges) opened the record.
    fn open_current(&mut self) -> HostResult<&mut OpenRecord> {
        self.require_privileges()?;
        let window = self.window()?;
        let (table, id) = (
            window.table,
            window.record().ok_or(ErrorCode::RECORD_MISSING)?,
        );
        if self
            .record
            .as_ref()
            .is_some_and(|r| r.table == table && r.id == id)
        {
            return Ok(self.record.as_mut().expect("checked"));
        }
        self.plan(table)?;
        self.commit_open(Entry::script(false, false))?;
        // A record the transaction already changed reopens with its
        // changes (the transaction keeps its own copy for Revert Record);
        // its lock is held.
        match self.pending(table, id) {
            Some(Change::Write(record)) => {
                let record = record.clone();
                return Ok(self.record.insert(record));
            }
            Some(Change::Delete) => return Err(ErrorCode::RECORD_MISSING.into()),
            None => {}
        }
        self.lock_record(table, id)?;
        let Some(node) = self.committed_node(table, id) else {
            self.unlock_record(table, id);
            return Err(ErrorCode::RECORD_MISSING.into());
        };
        let context = Box::new(self.step_context());
        Ok(self.record.insert(OpenRecord {
            table,
            id,
            node,
            new: false,
            changed: BTreeSet::new(),
            ruled: None,
            serials: BTreeMap::new(),
            context,
        }))
    }

    /// Commit the open record (leaving a record, changing layouts, Commit
    /// Records) with its table's rules; inside a transaction it joins the
    /// transaction, whose commit applies them. A record the rules refuse
    /// stays open and the step fails with their error.
    fn commit_open(&mut self, entry: Entry) -> HostResult {
        let Some(record) = self.record.take() else {
            return Ok(());
        };
        let changed = record.new || !record.changed.is_empty();
        if let Some(transaction) = self.transaction.as_mut() {
            if changed {
                transaction.insert((record.table, record.id), Change::Write(record));
            } else {
                self.unlock_record(record.table, record.id);
            }
            return Ok(());
        }
        if !changed {
            self.unlock_record(record.table, record.id);
            return Ok(());
        }
        // The rules and the write work on a copy: a commit the rules
        // refuse, or one the file cannot be saved with, leaves the record
        // open as the script changed it (and committing it again applies
        // the rules to those changes again).
        let mut ready = record.clone();
        let committed = self.apply_rules(&mut ready, entry).and_then(|_| {
            let key = (ready.table, ready.id);
            self.write(vec![(key, Change::Write(ready))])
        });
        if committed.is_err() {
            self.record = Some(record);
        }
        committed
    }

    /// Drop the open record's changes: a new record is gone, an edited one
    /// keeps its stored values.
    fn drop_open(&mut self) {
        self.focus = None;
        if let Some(record) = self.record.take() {
            self.unlock_record(record.table, record.id);
            if record.new {
                if let Some(window) = self.window.as_mut() {
                    window.found.retain(|r| *r != record.id);
                    window.current = window.current.min(window.found.len().saturating_sub(1));
                }
            }
        }
    }

    /// What `Get` answers while record rules compute values: the account
    /// (also the user name on the server), the server's clock in the
    /// storage forms the rules read (en-US, as the client's), a new UUID.
    /// The rules' calculations stop with the script, refusing the write,
    /// and have the script's limits.
    fn record_environment(&self) -> RecordEnvironment {
        let control = self.control.clone();
        let environment = crate::recalc::record_environment(&self.account, self.uuid());
        RecordEnvironment {
            containers: Some(self.container_reader()),
            interrupted: Some(Arc::new(move || control.aborted().is_some())),
            limits: self.calc_limits,
            context: self.write_context(&environment.timestamp),
            ..environment
        }
    }

    /// What the session answers for the record rules' context selectors
    /// ([`rules::WriteContext`]), as its own calculations read them
    /// (`get_answer`, `get_value`): its window's layout number (none without
    /// a window), its current privilege set ([Full Access] in a step running
    /// with full access; a set it cannot name is not answered), and the
    /// server's clock, `clock`, as the host's.
    fn write_context(&self, clock: &str) -> rules::WriteContext {
        rules::WriteContext {
            host_timestamp: Some(clock.to_owned()),
            ..self.step_context()
        }
    }

    /// [`Self::write_context`] without the clock: what the current step
    /// answers for the layout number and the privilege set, as an
    /// [`OpenRecord`] keeps it.
    fn step_context(&self) -> rules::WriteContext {
        let cx = self.cx();
        let answer = |selector| {
            self.get_answer(selector)
                .unwrap_or_else(|| Ok(self.get_value(selector)))
                .ok()
                .map(|value| value.to_text(&cx).into_string())
        };
        let layout = match answer(selector::LAYOUT_NUMBER) {
            Some(text) if text.is_empty() => rules::WriteLayout::NoWindow,
            Some(text) => text
                .parse()
                .map_or(rules::WriteLayout::Unknown, rules::WriteLayout::Number),
            None => rules::WriteLayout::Unknown,
        };
        rules::WriteContext {
            layout,
            privilege_set: answer(om_data::host::session::selector::CURRENT_PRIVILEGE_SET_NAME)
                .filter(|name| !name.is_empty()),
            host_timestamp: None,
            // A record a script makes is given its position by New Record
            // (#298); other writes' record number is unmeasured.
            record_number: None,
        }
    }

    /// Apply the table's rules to a record about to be written: auto-enter
    /// values, stored calculations, validation and unique values
    /// (`om_data::rules`, auto-entry.md); the computed cells go into the
    /// record. A record the rules already ran on ([`Ruled`]) gets them only
    /// for the cells changed since, over what they left (nothing to do when
    /// none changed): an imported row keeps what the import's own options
    /// gave it. Returns the cells the rules wrote.
    fn apply_rules(&mut self, record: &mut OpenRecord, entry: Entry) -> HostResult<BTreeSet<u64>> {
        // The place New Record gave a new record is its number only while
        // it is still there, in the window's unsorted found set: its
        // creation and its commit then read the same place. Otherwise
        // (records before it omitted, the found set sorted or found again,
        // another table's) what FileMaker stores is unmeasured (#298).
        if let Some(number) = record.context.record_number {
            let at = usize::try_from(number).ok().and_then(|n| n.checked_sub(1));
            let there = self.window().is_ok_and(|w| {
                w.sort.is_none() && at.and_then(|at| w.found.get(at)) == Some(&record.id)
            });
            if !there {
                record.context.record_number = None;
            }
        }
        let plan = self.plan(record.table)?;
        let (mode, existing, cells) = match &record.ruled {
            Some(ruled) if ruled.since.is_empty() => return Ok(BTreeSet::new()),
            Some(ruled) => (
                WriteMode::Update,
                ruled.base.clone().unwrap_or_else(|| texts(&record.node)),
                ruled.since.clone(),
            ),
            None if record.new => (WriteMode::Create, BTreeMap::new(), record.changed.clone()),
            None => {
                let stored = self.committed_node(record.table, record.id);
                (
                    WriteMode::Update,
                    stored.as_ref().map(texts).unwrap_or_default(),
                    record.changed.clone(),
                )
            }
        };
        let changes: BTreeMap<u64, Option<String>> = cells
            .iter()
            .map(|field| {
                let text = record
                    .node
                    .child(&uint_segment(*field))
                    .and_then(values::cell_text);
                (*field, text)
            })
            .collect();
        // What the write's lookups copy, read from the session's records as
        // the script's calculations read them.
        let mut environment = self.record_environment();
        // The step that last changed the record, not the one committing it.
        environment.context = rules::WriteContext {
            host_timestamp: environment.context.host_timestamp.take(),
            ..(*record.context).clone()
        };
        // The session's global values, which an auto-entered or validation
        // formula reads of a global field (schema-path.md 5.17).
        if plan.reads_globals() {
            environment.globals = Some(self.session_globals(record.table));
        }
        // Serials not taken yet: a new record's on creation were taken by
        // New Record.
        environment.serials = record.serials.clone();
        let missing: Vec<_> = plan
            .serials_for(mode, entry)
            .into_iter()
            .filter(|serial| !record.serials.contains_key(&serial.field))
            .collect();
        environment
            .serials
            .extend(self.reserve_serials(record.table, missing)?);
        // Read calculated lookup keys only once their dependencies have
        // settled. Match the provider's bounded demand protocol; exhausted
        // or unsupported demands preserve the evaluator's explicit error.
        let needs = Arc::new(rules::RelatedNeeds::default());
        environment.related_needs = Some(Arc::clone(&needs));
        let mut keyed = BTreeMap::new();
        let mut rounds = 4;
        let computed = loop {
            environment.lookups = plan
                .read_lookups_with(
                    mode,
                    &existing,
                    &changes,
                    entry,
                    &related::SessionSource { host: self },
                    // Its own row is left out of its table's self-joins (#1035).
                    // An imported row's rules see what the import left, not its
                    // stored row: it names no record, and answers no self-join.
                    (
                        &keyed,
                        match record.ruled {
                            Some(_) => rules::Written::Unknown,
                            None => rules::Written::held(mode, record.id),
                        },
                    ),
                )
                .map_err(|e| {
                    info!(
                        "{:?}: record {} of table {:#x} refused: {e}",
                        self.hosted.name, record.id, record.table
                    );
                    rule_error(&e)
                })?;
            let computed = plan
                .compute(mode, &existing, &changes, &environment, entry)
                .map_err(|e| {
                    info!(
                        "{:?}: record {} of table {:#x} refused: {e}",
                        self.hosted.name, record.id, record.table
                    );
                    rule_error(&e)
                });
            let more = needs.take();
            if more.is_empty() || rounds == 0 {
                break computed?;
            }
            rounds -= 1;
            keyed.extend(more);
        };
        for (field, text) in &computed {
            let key = uint_segment(*field);
            match text {
                Some(text) => record.node.set_child(values::cell(&key, text)),
                None => {
                    record.node.remove(&[&key]);
                }
            }
        }
        // A related-values list is computed for the written record only by
        // the desktop's provider (om-provider `related_lists.rs`): a value
        // the rules check against one refuses the write here (3).
        for (cell, text) in computed.iter().chain(&changes) {
            if text.as_deref().is_some_and(|t| !t.is_empty())
                && plan.related_value_list(*cell, entry).is_some()
            {
                return Err(related_list_refused());
            }
        }
        // An imported value is not Set Field's, which checks a value list of
        // a field's values as it is entered: the import checks it here.
        if entry.keep_written {
            for (cell, text) in &changes {
                let Some(text) = text.as_deref().filter(|t| !t.is_empty()) else {
                    continue;
                };
                if let Some((occurrence, source)) = plan.value_list_field(*cell, entry) {
                    if !self.field_holds(occurrence, source, text) {
                        let is_field = self
                            .schema
                            .catalog
                            .table(record.table)
                            .is_some_and(|t| t.field(*cell).is_some());
                        let field = if is_field { *cell } else { *cell >> 8 };
                        return Err(rule_error(&plan.refusal(field, NOT_IN_LIST.0)));
                    }
                }
            }
        }
        // Each repetition the write changed is checked against every
        // repetition of the other records; one record may repeat a value
        // (script oracle).
        for field in plan.unique_fields_for(entry) {
            let cells: Vec<u64> = plan.cells(field).collect();
            for cell in &cells {
                let changed = computed.contains_key(cell) || changes.contains_key(cell);
                let text = record
                    .node
                    .child(&uint_segment(*cell))
                    .and_then(values::cell_text);
                if let Some(text) = text.filter(|t| changed && !t.is_empty()) {
                    if self.value_held_elsewhere(record, field, &cells, &text) {
                        info!(
                            "{:?}: record {} of table {:#x} refused: field {field} must be unique",
                            self.hosted.name, record.id, record.table
                        );
                        return Err(NOT_UNIQUE.into());
                    }
                }
            }
        }
        // An existing value: each changed, non-empty repetition must be in
        // some repetition of another record (script oracle s18: 505 from
        // the commit, also for a field validated only during data entry).
        for field in plan.existing_fields_for(entry) {
            let cells: Vec<u64> = plan.cells(field).collect();
            for cell in &cells {
                let changed = computed.contains_key(cell) || changes.contains_key(cell);
                let text = record
                    .node
                    .child(&uint_segment(*cell))
                    .and_then(values::cell_text);
                if let Some(text) = text.filter(|t| changed && !t.is_empty()) {
                    if !self.value_held_elsewhere(record, field, &cells, &text) {
                        info!(
                            "{:?}: record {} of table {:#x} refused: field {field} needs an \
                             existing value",
                            self.hosted.name, record.id, record.table
                        );
                        return Err(NOT_EXISTING.into());
                    }
                }
            }
        }
        let written: BTreeSet<u64> = computed.keys().chain(changes.keys()).copied().collect();
        if let Some(ruled) = &mut record.ruled {
            ruled.written.extend(&written);
            ruled.base = None;
            ruled.since.clear();
        }
        Ok(written)
    }

    /// A value for each of `serials` of a new record of `table`
    /// (`RecordPlan::serials_for`), each reserved as a client's op72
    /// reserves it (write-path.md 3.2): the field's next value advanced by
    /// its increment and the file saved, outside the commit, so a commit
    /// that fails or a transaction reverted does not give it back
    /// (`revert-transaction.html`) and no other session or client gets it.
    fn reserve_serials(
        &self,
        table: u64,
        serials: impl IntoIterator<Item = rules::Serial>,
    ) -> HostResult<BTreeMap<u64, String>> {
        let mut reserved = BTreeMap::new();
        for rules::Serial {
            field, increment, ..
        } in serials
        {
            let reply = writes::get_serial(
                &self.hosted,
                &GetSerial {
                    table: vec![uint_segment(table)],
                    field: vec![uint_segment(field)],
                    value: String::new(),
                    increment,
                    flags: SERIAL_INCREMENT,
                },
            );
            self.hosted.save();
            if reply.status != 0 {
                info!(
                    "{:?}: no serial number for field {field} of table {table:#x}: error {}",
                    self.hosted.name, reply.status
                );
                return Err(ErrorCode::UNAVAILABLE.into());
            }
            reserved.insert(field, reply.value);
        }
        Ok(reserved)
    }

    /// Whether field `field` of `occurrence`'s table holds `text` in some
    /// record and repetition (a value list of the field's values): its
    /// value index when it has one, else its stored values, as committed.
    /// Text compares as index keys, so case does not matter in English
    /// (script oracle s18); keyed as the field's index keys it, English
    /// where OpenMaker has no exact form (#159).
    fn field_holds(&self, occurrence: u64, field: u64, text: &str) -> bool {
        let Some((table, def)) = self.schema.field(occurrence, field) else {
            return false;
        };
        let keys = FieldOptions::decode(&def.options)
            .and_then(|o| rules::checked_text_keys(o.index_language()))
            .unwrap_or(TextKeys::Collated);
        let key = keys.key(text).unwrap_or_default();
        let store = self.hosted.read_store();
        let (t, f) = (uint_segment(table), uint_segment(field));
        if let Some(index) = store.node(&[&t, &[INDEXES], &f, &[VALUE_INDEX]]) {
            return index.child(&key).is_some();
        }
        let cells: Vec<u64> = (1..=Self::repetitions(def))
            .filter_map(|rep| cell_id(field, rep))
            .collect();
        store.node(&[&t, &[RECORDS]]).is_some_and(|records| {
            records.children.iter().any(|record| {
                cells.iter().any(|cell| {
                    record
                        .child(&uint_segment(*cell))
                        .and_then(values::cell_text)
                        .is_some_and(|v| keys.key(&v).as_ref() == Some(&key))
                })
            })
        })
    }

    /// Whether a record other than `record` holds `text` in a repetition
    /// (`cells`) of `field` (a unique or existing-value text field, keyed
    /// in its index language, query-program.md 7.2): in the file's value
    /// index, which holds every repetition's keys, or among the records
    /// this session changed.
    fn value_held_elsewhere(
        &self,
        record: &OpenRecord,
        field: u64,
        cells: &[u64],
        text: &str,
    ) -> bool {
        let table = record.table;
        // The record rules plan such a field only in a language whose keys
        // `checked_text_keys` builds.
        let keys = self
            .schema
            .catalog
            .table(table)
            .and_then(|t| t.field(field))
            .and_then(|f| f.decoded_options())
            .and_then(|o| rules::checked_text_keys(o.index_language()))
            .unwrap_or(TextKeys::Collated);
        let key = keys.key(text).unwrap_or_default();
        let holds = |node: &OwnedNode| {
            cells.iter().any(|cell| {
                node.child(&uint_segment(*cell))
                    .and_then(values::cell_text)
                    .is_some_and(|v| keys.key(&v).as_ref() == Some(&key))
            })
        };
        let mut holders: BTreeSet<u64> = {
            let store = self.hosted.read_store();
            let (t, f) = (uint_segment(table), uint_segment(field));
            match store.node(&[&t, &[INDEXES], &f, &[VALUE_INDEX]]) {
                Some(index) => index
                    .child(&key)
                    .and_then(|ids| RidList::from_owned(ids).ok())
                    .map(|ids| ids.iter().collect())
                    .unwrap_or_default(),
                None => {
                    drop(store);
                    self.committed_records(table)
                        .into_iter()
                        .filter(|id| self.committed_node(table, *id).is_some_and(|n| holds(&n)))
                        .collect()
                }
            }
        };
        // The records the transaction changed, as it holds them: through
        // its index of their values, not one by one.
        if let Some(transaction) = &self.transaction {
            holders.retain(|id| !transaction.changes.contains_key(&(table, *id)));
            holders.extend(transaction.holders(table, field, cells, (keys, &key)));
        }
        if let Some(open) = self.record.as_ref().filter(|r| r.table == table) {
            holders.remove(&open.id);
            if holds(&open.node) {
                holders.insert(open.id);
            }
        }
        holders.remove(&record.id);
        !holders.is_empty()
    }

    /// Write `changes` into the file as one commit (write-path.md 3.4, 5):
    /// all or nothing, saved to disk before it counts; then tell the
    /// clients registered on the file (6.2-6.4).
    fn write(&mut self, changes: Vec<((u64, u64), Change)>) -> HostResult {
        self.write_with(changes, false)
    }

    /// [`ServerHost::write`] of Delete All Records or Truncate Table
    /// (`bulk_delete`): a table they leave empty loses its library.
    fn write_with(&mut self, changes: Vec<((u64, u64), Change)>, bulk_delete: bool) -> HostResult {
        let mut exhausted = None;
        let records = changes
            .into_iter()
            .map(|((table, id), change)| match change {
                Change::Write(mut record) => {
                    if !record.new {
                        // An edit uploads the whole record with its
                        // modification count one higher (record-writes.md).
                        if modcount::bump(&mut record.node, 1).is_none() {
                            exhausted.get_or_insert(id);
                        }
                    }
                    RecordWrite {
                        table,
                        id,
                        new: record.new,
                        uploads: self.uploads(table, &record.node),
                        record: Some(record.node),
                    }
                }
                Change::Delete => RecordWrite {
                    table,
                    id,
                    new: false,
                    record: None,
                    uploads: Vec::new(),
                },
            })
            .collect();
        // A count that cannot advance refuses the whole commit before
        // anything is written: changed without a new count, a client's copy
        // of the record would not show as stale (#361).
        if let Some(id) = exhausted {
            warn!(
                "{:?}: script commit refused: record {id}'s modification count is the largest \
                 a serial key holds",
                self.hosted.name
            );
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        match writes::commit_records(&self.hosted, self.user, records, bulk_delete) {
            Ok(changes) => {
                writes::notify(
                    &self.hosted,
                    &self.shared.mailboxes,
                    self.user.connection,
                    &changes,
                );
                Ok(())
            }
            Err((code, reason)) => {
                warn!("{:?}: script commit refused: {reason}", self.hosted.name);
                Err(ErrorCode(code as i32).into())
            }
        }
    }

    /// End the open transaction without writing it: records it created are
    /// gone, those it deleted are back, its locks are released.
    fn discard_transaction(&mut self) {
        let changes: Vec<((u64, u64), Change)> = self
            .transaction
            .take()
            .map(|t| t.changes.into_iter().collect())
            .unwrap_or_default();
        let (mut created, deleted) = created_and_deleted(&changes);
        if let Some(record) = self.record.take() {
            if record.new {
                created.push((record.table, record.id));
            }
        }
        self.undo(created, deleted);
    }

    /// The window after a transaction that was not written: `created`
    /// records leave its found set, `deleted` ones return; the
    /// transaction's locks are released.
    fn undo(&mut self, created: Vec<(u64, u64)>, deleted: Vec<(u64, u64)>) {
        self.hosted.lock_table().release_txn(self.user);
        if let Some(window) = self.window.as_mut() {
            // The restored records go back in creation order (a sorted
            // found set is then partly sorted, which is not tracked).
            let table = window.table;
            window.found.retain(|r| !created.contains(&(table, *r)));
            let restored: Vec<u64> = deleted
                .into_iter()
                .filter(|(t, _)| *t == table)
                .map(|(_, id)| id)
                .collect();
            if !restored.is_empty() {
                window.found.extend(restored);
                window.found.sort_unstable();
                window.found.dedup();
            }
            window.current = window.current.min(window.found.len().saturating_sub(1));
        }
    }

    /// Leave the record (and its field): commit it.
    fn leave(&mut self) -> HostResult {
        self.focus = None;
        self.request_open = false;
        self.commit_open(Entry::script(false, false))
    }

    /// The field a step targets, checked: its table and definition.
    fn target(&self, field: FieldRef) -> HostResult<(u64, Field)> {
        let (table, def) = self
            .schema
            .field(field.occurrence, field.field)
            .ok_or(ErrorCode::FIELD_MISSING)?;
        Ok((table, def.clone()))
    }

    fn is_global(def: &Field) -> bool {
        FieldOptions::decode(&def.options).is_some_and(|o| o.options & GLOBAL_STORAGE != 0)
    }

    fn repetitions(def: &Field) -> u32 {
        FieldOptions::decode(&def.options).map_or(1, |o| u32::from(o.max_repetitions.max(1)))
    }

    /// Store `value` in `field` of the current record, or as a find
    /// criterion in Find mode.
    fn store_value(&mut self, field: FieldRef, value: Value) -> HostResult {
        let (table, def) = self.target(field)?;
        if field.repetition > Self::repetitions(&def) {
            return Err(REPETITION_INVALID.into());
        }
        // Find mode takes a criterion in a calculation field too, never in
        // a summary field (script-runtime.md 13.19).
        let find_mode = self.window.as_ref().is_some_and(|w| w.mode == Mode::Find);
        let settable = match def.kind {
            FieldKind::Normal => true,
            FieldKind::Calculation => find_mode && !Self::is_global(&def),
            _ => false,
        };
        if !settable {
            return Err(FIELD_LOCKED.into());
        }
        let ty = values::value_type(def.data_type);
        if Self::is_global(&def) {
            self.require_privileges()?;
            self.globals
                .insert((table, field.field, field.repetition), value);
            return Ok(());
        }
        let window = self.window()?;
        if window.occurrence != field.occurrence {
            // A related record: relationships are not followed yet.
            return Err(HostError::Unsupported);
        }
        if window.mode == Mode::Find {
            let text = value.to_text(&self.cx()).into_string();
            let current = window.current;
            let request = self
                .requests
                .get_mut(current)
                .ok_or(ErrorCode::RECORD_MISSING)?;
            request.set(field.field, text);
            return Ok(());
        }
        let cell = cell_id(field.field, field.repetition).ok_or(HostError::Unsupported)?;
        let container = def.data_type == DataType::Container;
        let stored = if container {
            // Data, a reference or text ([`containers`]).
            self.container_text(table, &value)
        } else {
            values::to_stored(ty, &value, &self.cx())
        };
        // The value as Set Field gives it, which its checks read (a
        // two-digit year is not a four-digit one once stored as a date).
        let entered = value.to_text(&self.cx()).into_string();
        self.open_current()?;
        if let Some(text) = stored.as_ref().filter(|_| !container).map(|_| &entered) {
            // The rules checked as a value is entered (maximum characters,
            // strict data types, range, value list) are Set Field's own
            // errors, the field keeping its value, for a field validated
            // Always (script oracle s17, s18).
            let skip = self
                .transaction
                .as_ref()
                .is_some_and(|t| t.options.skip_validation);
            let entry = Entry::script(skip, false);
            let plan = self.plan(table)?;
            plan.check_entry(cell, text, entry)
                .map_err(|e| rule_error(&e))?;
            if plan.related_value_list(cell, entry).is_some() {
                return Err(related_list_refused());
            }
            if let Some((occurrence, source)) = plan.value_list_field(cell, entry) {
                if !self.field_holds(occurrence, source, text) {
                    info!(
                        "{:?}: Set Field refused: field {} must hold a value of its value list",
                        self.hosted.name, field.field
                    );
                    return Err(rule_error(&plan.refusal(field.field, NOT_IN_LIST.0)));
                }
            }
        }
        // Setting a lookup's match field performs the lookup also when the
        // value stays (auto-entry.md, Lookups, battery s41): the commit
        // gives it to the rules as set. Open Transaction's "Skip auto-enter
        // options" performs no lookup, so then nothing changes.
        let skip_auto_enter = self
            .transaction
            .as_ref()
            .is_some_and(|t| t.options.skip_auto_enter);
        let lookup_match = !skip_auto_enter
            && self
                .plan(table)
                .is_ok_and(|plan| plan.is_lookup_match(cell));
        let context = self.step_context();
        let record = self.record.as_mut().expect("opened");
        let key = uint_segment(cell);
        let before = record.node.child(&key).cloned();
        record.before_change();
        // The place New Record gave the record stays its own.
        *record.context = rules::WriteContext {
            record_number: record.context.record_number,
            ..context
        };
        match stored {
            Some(text) => record.node.set_child(values::cell(&key, &text)),
            None => {
                record.node.remove(&[&key]);
            }
        }
        if record.node.child(&key) != before.as_ref() || lookup_match {
            record.changed_cell(cell);
        }
        Ok(())
    }

    fn value_of(&self, field: FieldRef) -> HostResult<Value> {
        let window = self.window()?;
        let record = window.record().ok_or(ErrorCode::RECORD_MISSING)?;
        self.read_field(field.occurrence, record, field.field, field.repetition)
            .map_err(|e| HostError::Error(ErrorCode(e.code())))
    }

    /// Delete records of the window's table, from the found set too.
    /// Inside a transaction the deletions wait for Commit Transaction.
    /// Deletions the file cannot be saved with fail: the records stay (an
    /// open one open), only records never committed are gone.
    fn delete(&mut self, ids: &[u64], bulk: bool) -> HostResult {
        self.require_privileges()?;
        let table = self.window()?.table;
        let mut deleted = Vec::new();
        let mut created = Vec::new();
        let mut locked_here = Vec::new();
        // The open record among them, when it is in the file.
        let mut reopen = None;
        for &id in ids {
            let open = self.record.take_if(|r| r.table == table && r.id == id);
            let pending = self.pending(table, id);
            if matches!(pending, Some(Change::Delete)) {
                continue;
            }
            let held = matches!(pending, Some(Change::Write(_)));
            if open.as_ref().is_some_and(|r| r.new)
                || matches!(pending, Some(Change::Write(r)) if r.new)
            {
                // Never committed: nothing to delete in the file. A record
                // created in a transaction keeps its id's lock until the
                // transaction ends.
                match self.transaction.as_mut() {
                    Some(transaction) => {
                        transaction.remove(&(table, id));
                    }
                    None => self.unlock_record(table, id),
                }
                created.push(id);
                continue;
            }
            if open.is_none() && !held {
                if let Err(e) = self.lock_record(table, id) {
                    self.keep_undeleted(table, &created, &locked_here, reopen);
                    return Err(e);
                }
                locked_here.push(id);
            }
            reopen = reopen.or(open);
            deleted.push(id);
        }
        if self.transaction.is_none() && !deleted.is_empty() {
            let changes = deleted.iter().map(|id| ((table, *id), Change::Delete));
            if let Err(e) = self.write_with(changes.collect(), bulk) {
                self.keep_undeleted(table, &created, &locked_here, reopen);
                return Err(e);
            }
        }
        let window = self.window_mut()?;
        window.found.retain(|r| !ids.contains(r));
        window.current = window.current.min(window.found.len().saturating_sub(1));
        if let Some(transaction) = &mut self.transaction {
            for id in deleted {
                transaction.insert((table, id), Change::Delete);
            }
        }
        Ok(())
    }

    /// After a deletion that failed: the records `created` here (never
    /// committed, now gone) leave the found set, the locks it took are
    /// released and the open record it would have deleted is open again.
    fn keep_undeleted(
        &mut self,
        table: u64,
        created: &[u64],
        locked_here: &[u64],
        reopen: Option<OpenRecord>,
    ) {
        for id in locked_here {
            self.unlock_record(table, *id);
        }
        if reopen.is_some() {
            self.record = reopen;
        }
        if let Some(window) = self.window.as_mut() {
            window.found.retain(|r| !created.contains(r));
            window.current = window.current.min(window.found.len().saturating_sub(1));
        }
    }

    /// The records of the window's table one find request matches
    /// (query-program.md 6): every criterion holds (AND). Each criterion's
    /// finders run as one SimpleFind (`om_query::find::plan`); where they
    /// only select candidates (wildcards, phrases, `?`, `~`, partial dates),
    /// the candidates' stored values are tested with the criterion, as the
    /// desktop client does. A criterion on an unstored calculation has no
    /// index: the calculation is computed on each candidate and tested
    /// (script-runtime.md 13.19).
    fn matches(&mut self, request: &Request) -> Result<RidList, FindError> {
        let window = self.window.as_ref().ok_or(FindError::Unsupported)?;
        let (table, occurrence) = (window.table, window.occurrence);
        let order = self.locale.date_order;
        let (year, month, day) = self.today.ymd();
        let context = find::FindContext::new(order, Some((year as i32, month, day)));
        let mut keep = Vec::new();
        let mut remove = Vec::new();
        let mut filters = Vec::new();
        let mut evaluated: Vec<(u64, find::Criterion)> = Vec::new();
        for (field, text) in &request.criteria {
            let (_, def) = self
                .schema
                .field(occurrence, *field)
                .ok_or(FindError::Unsupported)?;
            let criterion = find::Criterion::parse(text, def.data_type, &context)
                .map_err(|_| FindError::Unsupported)?;
            let Some(criterion) = criterion else {
                continue;
            };
            if om_data::found::findable(def) == Findable::Evaluated {
                evaluated.push((*field, criterion));
                continue;
            }
            let plan = find::plan(table, *field, &criterion);
            for finder in plan.finders {
                if finder.keep {
                    keep.push(finder.bytes);
                } else {
                    remove.push(finder.bytes);
                }
            }
            if !plan.exact {
                filters.push((*field, criterion));
            }
        }
        let canceled = || FindError::Query(ErrorCode::CANCELED.0 as u16);
        // The finders are ANDed, so they can run a batch at a time.
        let mut found: Option<RidList> = None;
        for batch in keep.chunks(FINDER_BATCH) {
            if self.aborted() {
                return Err(canceled());
            }
            let batch = self.query(table, batch)?;
            found = Some(match found {
                Some(found) => found.and(&batch),
                None => batch,
            });
        }
        let mut matched = match found {
            Some(found) => found,
            None => self.all_records(table).into_iter().collect(),
        };
        for finder in remove {
            matched = matched.and_not(&self.query(table, &[finder])?);
        }
        if !filters.is_empty() {
            // A duplicates criterion among them already has the host's
            // FindDuplicates finder applied.
            let mut kept = RidList::new();
            for id in matched.iter() {
                // Testing one record can cost a criterion's length times
                // the value's (a wildcard pattern), so every record checks.
                if self.aborted() {
                    return Err(canceled());
                }
                let holds = filters.iter().all(|(field, criterion)| {
                    let value = self.stored_text(table, id, *field);
                    criterion.matches(&value, order, &|_| true)
                });
                if holds {
                    kept.insert(id);
                }
            }
            matched = kept;
        }
        if !evaluated.is_empty() {
            let all = if evaluated.iter().any(|(_, c)| c.finds_duplicates()) {
                self.all_records(table)
            } else {
                Vec::new()
            };
            let criteria: Vec<find::Criterion> = evaluated.iter().map(|(_, c)| c.clone()).collect();
            let cx = self.cx();
            let kept = om_data::found::filter_evaluated(
                &criteria,
                matched.iter(),
                &all,
                order,
                |record, i| -> Result<String, FindError> {
                    // Each value is a calculation computed on the record.
                    if self.aborted() {
                        return Err(canceled());
                    }
                    let value = self
                        .read_field(occurrence, record, evaluated[i].0, 1)
                        .unwrap_or_else(|_| Value::invalid());
                    Ok(value.to_text(&cx).into_string())
                },
            )?;
            matched = kept.into_iter().collect();
        }
        Ok(matched)
    }

    /// Whether the script was aborted or ran out of time: long work inside
    /// a step stops then with error 1, a scan or sort every
    /// [`ABORT_CHECK`] records, a find at every [`FINDER_BATCH`] and every
    /// record it tests, a calculation as `CalcHost::interrupted` says.
    fn aborted(&self) -> bool {
        self.control.aborted().is_some()
    }

    /// A stored cell of `def`, a field of `table`, as calculations read
    /// it: converted to the field's type in the file's formats
    /// ([`values::decode`]); for a container field the data of the library
    /// member it names (containers.md 1.4, [`Store::container`]): its file
    /// name as the text, its streams and whether they are intact.
    fn stored_value(&self, table: u64, def: &Field, cell: Option<&OwnedNode>) -> Value {
        if def.data_type != DataType::Container {
            return values::decode(values::value_type(def.data_type), cell, &self.cx());
        }
        match cell.and_then(values::cell_text).filter(|t| !t.is_empty()) {
            Some(text) => self.container_value(table, &text),
            None => Value::empty(ValueType::Container),
        }
    }

    /// A global field's value in this session: the one a step set, else
    /// the file's stored value `[T, 08, F]` (calc-values.md 13, "Global
    /// values"), which every session starts from, as on FileMaker Server
    /// (no other session's changes are seen).
    /// The session's values of `table`'s global fields, laid out like a
    /// record: those a step set, the file's stored ones otherwise. A
    /// container global carries its text only.
    fn session_globals(&self, table: u64) -> Arc<RecordValues> {
        let mut values = RecordValues::default();
        let Some(def) = self.schema.catalog.table(table) else {
            return Arc::new(values);
        };
        for f in def.fields.iter().filter(|f| ServerHost::is_global(f)) {
            for rep in 1..=ServerHost::repetitions(f).min(255) {
                let value = self.global_value(table, f, rep);
                if f.data_type == DataType::Container {
                    if let Value::Container(c) = &value {
                        values.set(
                            f.id,
                            rep,
                            Value::Container(om_calc::Container {
                                text: c.text.clone(),
                                ..Default::default()
                            }),
                        );
                    }
                    continue;
                }
                values.set(f.id, rep, value);
            }
        }
        Arc::new(values)
    }

    fn global_value(&self, table: u64, def: &Field, repetition: u32) -> Value {
        if let Some(value) = self.globals.get(&(table, def.id, repetition)) {
            return value.clone();
        }
        let Some(cell) = cell_id(def.id, repetition) else {
            return Value::empty(values::value_type(def.data_type));
        };
        let stored = {
            let store = self.hosted.read_store();
            let (t, c) = (uint_segment(table), uint_segment(cell));
            store.node(&[&t, &[GLOBALS], &c]).cloned()
        };
        self.stored_value(table, def, stored.as_ref())
    }

    /// What a field reads on [`NO_RECORD`]: in Find mode the current
    /// request's criterion for a field of the window's occurrence, as text
    /// (script oracle s18-find: `b` after Set Field of `b`), else the
    /// field's empty value.
    fn request_value(&self, occurrence: u64, field: u64, repetition: u32, ty: ValueType) -> Value {
        let criterion = self
            .window
            .as_ref()
            .filter(|w| w.mode == Mode::Find && w.occurrence == occurrence && repetition <= 1)
            .and_then(|w| self.requests.get(w.current))
            .and_then(|r| r.criteria.iter().find(|(f, _)| *f == field))
            .map(|(_, text)| text.as_str())
            .filter(|text| !text.is_empty());
        match criterion {
            Some(text) => Value::text(text),
            None => Value::empty(ty),
        }
    }

    /// A record's stored text of `field` (repetition 1) as the session
    /// sees it; empty when it has none.
    fn stored_text(&self, table: u64, id: u64, field: u64) -> String {
        self.cell(table, id, field)
            .and_then(|cell| om_hbam::Cell::from_owned(&cell).text())
            .and_then(Result::ok)
            .unwrap_or_default()
    }

    /// One SimpleFind through `om-query`, as the op86 handler runs it, on
    /// the records as the session sees them: the file's indexes answer for
    /// records the transaction did not change; those it changed are matched
    /// on their own ([`Store::scratch`]).
    fn query(&self, table: u64, finders: &[Vec<u8>]) -> Result<RidList, FindError> {
        let mut found = self.query_store(table, finders)?;
        let changed: Vec<(u64, Option<&OwnedNode>)> = self
            .pending_in(table)
            .map(|((_, id), change)| match change {
                Change::Write(r) => (*id, Some(&r.node)),
                Change::Delete => (*id, None),
            })
            .collect();
        if changed.is_empty() {
            return Ok(found);
        }
        found = found.and_not(&changed.iter().map(|(id, _)| *id).collect());
        let records: Vec<(u64, &OwnedNode)> = changed
            .iter()
            .filter_map(|(id, node)| Some((*id, (*node)?)))
            .collect();
        if records.is_empty() {
            return Ok(found);
        }
        let scratch = self
            .hosted
            .read_store()
            .scratch(&uint_segment(table), &records);
        let program = find::program(self.hosted.id, table, finders, None);
        let context = om_query::Context {
            file_id: self.hosted.id,
            connection_key: None,
        };
        let outcome = om_query::execute(&scratch, &program, &context);
        if let Some(error) = outcome.error {
            info!("{:?}: script find failed: {error}", self.hosted.name);
            return Err(FindError::Query(error.code()));
        }
        let matched = find::found(&outcome.slots).ok_or(FindError::Query(0xffff))?;
        Ok(found.or(&matched))
    }

    /// One SimpleFind on the file's own records and indexes.
    fn query_store(&self, table: u64, finders: &[Vec<u8>]) -> Result<RidList, FindError> {
        let program = find::program(self.hosted.id, table, finders, None);
        let context = om_query::Context {
            file_id: self.hosted.id,
            connection_key: None,
        };
        let (outcome, built) = {
            let store = self.hosted.read_store();
            let outcome = om_query::execute(&*store, &program, &context);
            (outcome, store.has_indexes_on_demand())
        };
        if built {
            // Indexes the find built on demand are stored (query-program.md
            // 7.1), as for a client's find.
            self.hosted.write_store().store_indexes_on_demand();
            self.hosted.save();
        }
        if let Some(error) = outcome.error {
            info!("{:?}: script find failed: {error}", self.hosted.name);
            return Err(FindError::Query(error.code()));
        }
        find::found(&outcome.slots).ok_or(FindError::Query(0xffff))
    }

    /// A field by `Table::Field` name, or a field of the window's table.
    fn field_named(&self, name: &str) -> Option<(u64, u64)> {
        let (occurrence, field) = match name.split_once("::") {
            Some((table, field)) => (self.schema.names.find_occurrence(table)?, field),
            None => (self.window.as_ref()?.occurrence, name),
        };
        Some((occurrence, self.schema.names.find_field(occurrence, field)?))
    }

    fn uuid(&self) -> String {
        crate::recalc::uuid_text(self.shared.random())
    }
}

/// The records a transaction's changes create and delete.
type Keys = Vec<(u64, u64)>;

fn created_and_deleted(changes: &[((u64, u64), Change)]) -> (Keys, Keys) {
    let mut created = Vec::new();
    let mut deleted = Vec::new();
    for (key, change) in changes {
        match change {
            Change::Write(record) if record.new => created.push(*key),
            Change::Write(_) => {}
            Change::Delete => deleted.push(*key),
        }
    }
    (created, deleted)
}

/// The text of every stored field of a record but its modification count:
/// the rules' view of it (the provider's `text_values`).
fn texts(node: &OwnedNode) -> BTreeMap<u64, String> {
    node.children
        .iter()
        .filter(|cell| cell.segment != [modcount::MOD_COUNT])
        .filter_map(|cell| {
            let id = om_hbam::slot::be_uint(&cell.segment)?;
            Some((id, values::cell_text(cell)?))
        })
        .collect()
}

/// The step error of a refused write: the rule's FileMaker error, or 3 for
/// a rule the server cannot enforce.
fn rule_error(error: &DataError) -> HostError {
    HostError::Error(ErrorCode(error.code.unwrap_or(rules::error::UNAVAILABLE)))
}

/// Error 3 for a value checked against a related-values list, which
/// OpenMaker Server does not compute for a written record.
fn related_list_refused() -> HostError {
    HostError::Error(ErrorCode(rules::error::UNAVAILABLE))
}

/// Today in the server's time zone.
fn today() -> Date {
    let now = jiff::Zoned::now();
    Date::from_ymd(i64::from(now.year()), now.month() as u32, now.day() as u32)
        .expect("a valid date")
}

/// The time of day in the server's time zone, in whole seconds.
fn now_time() -> Time {
    let now = jiff::Zoned::now();
    let seconds =
        i128::from(now.hour()) * 3600 + i128::from(now.minute()) * 60 + i128::from(now.second());
    Time::from_micros(seconds * 1_000_000, TimeStyle::Elapsed)
}

/// Microseconds since 1/1/0001 UTC.
fn utc_micros() -> i128 {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i128);
    EPOCH_MICROS + since_epoch
}

impl ServerHost {
    /// Facts about the session's account, the file's own, whose password
    /// authenticated it.
    fn authenticated_account_info(&self) -> om_data::host::AccountInfo {
        self.hosted
            .account(&self.account)
            .map(|account| account.authenticated_info())
            .unwrap_or_default()
    }

    /// What calculations a query computes are told by `Get`: the session's
    /// account and file, the server's clock.
    fn session_info(&self) -> SessionInfo {
        let file = self
            .hosted
            .name
            .rsplit_once('.')
            .map_or(self.hosted.name.as_str(), |(stem, _)| stem);
        SessionInfo {
            application_version: format!("Server {}", env!("CARGO_PKG_VERSION")),
            account_name: self.account.clone(),
            account_info: self.authenticated_account_info(),
            user_name: self.account.clone(),
            file_name: file.to_string(),
            file_path: format!("file:{}", self.hosted.path.display()),
            host_name: self.shared.identity.name.clone(),
            utc_offset: i64::from(jiff::Zoned::now().offset().seconds()),
            now: Timestamp::from_date_time(today(), now_time()),
            ..SessionInfo::for_this_machine()
        }
    }

    /// `ExecuteSQL` or `ExecuteSQLe` over the file as the session sees it.
    /// Field and record access are not decoded, so an account without
    /// [Full Access] reads nothing: error 9 before the statement is read.
    fn execute_sql(&mut self, variant: Variant, args: &[Value]) -> (Value, Option<CalcError>) {
        let mut locals = std::mem::take(&mut self.locals);
        let mut globals = std::mem::take(&mut self.global_vars);
        let vars = Vars::Scoped {
            locals: &mut locals,
            globals: &mut globals,
        };
        let result = self.execute_sql_with(variant, args, vars);
        self.locals = locals;
        self.global_vars = globals;
        result
    }

    /// [`Self::execute_sql`] with the variable maps `vars`.
    fn execute_sql_with(
        &self,
        variant: Variant,
        args: &[Value],
        vars: Vars<'_>,
    ) -> (Value, Option<CalcError>) {
        if !self.privileged() {
            return om_data::sql::calc::error_result(
                variant,
                &SqlError::Engine(NO_PRIVILEGES.into()),
            );
        }
        let Some((_, schema)) = self.schema.rules.clone() else {
            return om_data::sql::calc::error_result(variant, &SqlError::Engine(3));
        };
        let session = self.session_info();
        let source = SessionSource::new(self, &schema);
        let file = FileCalc::new(&schema, &source, self.today);
        let mut sql = FileSql::new(file, &session, vars);
        om_data::sql::calc::execute_sql(&mut sql, variant, args)
    }
}

/// The records a server session's query reads: the open record, then an
/// open transaction's version, then the file's ([`ServerHost::with_record`]),
/// decoded by field type once per query; the session's global values;
/// `field = value` as a SimpleFind on the file's indexes, which sees the
/// transaction's changes ([`ServerHost::query`]).
struct SessionSource<'h> {
    host: &'h ServerHost,
    schema: &'h CalcSchema,
    /// Records decoded by (table, record); `None`: no such record.
    records: std::cell::RefCell<DecodedRecords>,
    tables: std::cell::RefCell<HashMap<u64, Arc<[u64]>>>,
}

type DecodedRecords = HashMap<(u64, u64), Option<Arc<RecordValues>>>;

impl<'h> SessionSource<'h> {
    fn new(host: &'h ServerHost, schema: &'h CalcSchema) -> Self {
        SessionSource {
            host,
            schema,
            records: Default::default(),
            tables: Default::default(),
        }
    }

    fn values(&self, table: u64, node: &OwnedNode) -> RecordValues {
        let cx = self.host.cx();
        let mut values = RecordValues {
            mod_count: Some(modcount::serial_value(node.child(&[modcount::MOD_COUNT]))),
            ..RecordValues::default()
        };
        let Some(def) = self.schema.table(table) else {
            return values;
        };
        for f in def.fields.values() {
            if f.global
                || f.class == om_data::host::FieldClass::Summary
                || f.is_unstored_calculation()
            {
                continue;
            }
            for rep in 1..=f.repetitions.min(255) {
                // No cell can have a key past `u64::MAX` (#361).
                let Some(key) = cell_id(f.id, rep) else {
                    continue;
                };
                if let Some(cell) = node.child(&uint_segment(key)) {
                    values.set(f.id, rep, self.decode(table, f, cell, &cx));
                }
            }
        }
        values
    }
}

impl SessionSource<'_> {
    /// A stored cell of field `f` of `table` as a query reads it: a
    /// container field as its file name alone (execute-sql.md 8.1, what
    /// the column shows), without loading its data for every row.
    fn decode(&self, table: u64, f: &om_data::host::FieldDef, cell: &OwnedNode, cx: &Ctx) -> Value {
        if f.value_type != ValueType::Container {
            return values::decode(f.value_type, Some(cell), cx);
        }
        match values::cell_text(cell).filter(|t| !t.is_empty()) {
            Some(text) => Value::Container(om_calc::Container {
                text: om_calc::Text::plain(
                    self.host
                        .hosted
                        .read_store()
                        .container_name(&uint_segment(table), &text),
                ),
                ..Default::default()
            }),
            None => Value::empty(ValueType::Container),
        }
    }
}

impl CalcSource for SessionSource<'_> {
    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        if let Some(values) = self.records.borrow().get(&(table, record)) {
            return Ok(values.clone());
        }
        let values = self
            .host
            .with_record(table, record, |node| node.map(|n| self.values(table, n)))
            .map(Arc::new);
        self.records
            .borrow_mut()
            .insert((table, record), values.clone());
        Ok(values)
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        if let Some(ids) = self.tables.borrow().get(&table) {
            return Ok(Arc::clone(ids));
        }
        let ids: Arc<[u64]> = self.host.all_records(table).into();
        self.tables.borrow_mut().insert(table, Arc::clone(&ids));
        Ok(ids)
    }

    /// The session's global values (those a step set, the file's stored
    /// ones otherwise).
    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
        Ok(self.host.session_globals(table))
    }

    fn found_set(&self, _occurrence: u64, table: u64) -> Result<Arc<[u64]>, CalcError> {
        match &self.host.window {
            Some(w) if w.table == table => Ok(w.found.iter().copied().collect()),
            _ => self.records(table),
        }
    }

    /// The finds of Find mode for the whole value, with the open record,
    /// whose edits the file's indexes do not hold yet.
    fn lookup(&self, table: u64, field: u64, text: &str) -> Result<Option<Arc<[u64]>>, CalcError> {
        let Some((_, def)) = self
            .host
            .schema
            .catalog
            .table(table)
            .and_then(|t| Some((t.id, t.field(field)?)))
        else {
            return Ok(None);
        };
        if !matches!(def.data_type, DataType::Text | DataType::Date) {
            return Ok(None);
        }
        let criterion = format!("=={text}");
        let Ok(built) = find::finders(
            table,
            field,
            def.data_type,
            &criterion,
            self.host.locale.date_order,
        ) else {
            return Ok(None);
        };
        let finders: Vec<Vec<u8>> = built
            .into_iter()
            .filter(|f| f.keep)
            .map(|f| f.bytes)
            .collect();
        if finders.is_empty() {
            return Ok(None);
        }
        let Ok(found) = self.host.query(table, &finders) else {
            return Ok(None);
        };
        let mut ids: Vec<u64> = found.iter().collect();
        if let Some(open) = self.host.record.as_ref().filter(|r| r.table == table) {
            if !ids.contains(&open.id) {
                ids.push(open.id);
                ids.sort_unstable();
            }
        }
        Ok(Some(ids.into()))
    }

    /// A query stops when its script is aborted or runs out of time.
    fn interrupted(&self) -> bool {
        self.host.aborted()
    }
}

impl CalcHost for ServerHost {
    fn locale(&self) -> &Locale {
        &self.locale
    }

    /// A calculation stops when its script is aborted or runs out of time,
    /// as a query does.
    fn interrupted(&self) -> bool {
        self.aborted()
    }

    fn limits(&self) -> CalcLimits {
        self.calc_limits
    }

    fn container_data(
        &mut self,
        container: &om_calc::Container,
        _context: &Context,
    ) -> Result<om_calc::eval::StoredContainer, CalcError> {
        self.load_container(container)
    }

    fn today(&self) -> Date {
        self.today
    }

    fn names(&self) -> &dyn Names {
        &self.schema.names
    }

    /// Unstored and global calculations with their formulas, which the
    /// evaluator computes on the record ([`calculated`]).
    fn field(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        self.field_info(occurrence, field)
    }

    /// An unstored calculation whose referenced fields are all empty reads
    /// empty unless it evaluates always (calc-values.md 8.8), so `=` and
    /// `#` finds on it match FileMaker's (script-runtime.md 13.19).
    fn evaluate_always(&self, occurrence: u64, field: u64) -> bool {
        self.evaluates_always(occurrence, field)
    }

    fn field_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        let (table, def) = self
            .schema
            .field(occurrence, field)
            .ok_or(CalcError::FIELD_MISSING)?;
        if !self.privileged() {
            // Field access is not decoded: no field is readable (fail
            // closed), not even as FileMaker's "<No Access>".
            return Err(CalcError::from_code(NO_PRIVILEGES).expect("nonzero"));
        }
        let ty = values::value_type(def.data_type);
        if def.kind == FieldKind::Calculation
            && matches!(
                self.field_info(occurrence, field).map(|i| i.storage),
                Some(Storage::Unstored(_))
            )
        {
            return self.read_field(occurrence, record, field, repetition);
        }
        if Self::is_global(def) {
            return Ok(self.global_value(table, def, repetition));
        }
        if def.kind == FieldKind::Summary {
            return self.summary_value(occurrence, table, def, record, repetition);
        }
        if record == NO_RECORD {
            return Ok(self.request_value(occurrence, field, repetition, ty));
        }
        let Some(id) = cell_id(field, repetition) else {
            return Ok(Value::empty(ty));
        };
        let cell = self.cell(table, record, id);
        Ok(self.stored_value(table, def, cell.as_ref()))
    }

    /// Through the file's relationships (file-metadata.md 4.3); nothing
    /// is related to [`NO_RECORD`].
    fn relationship_origin(&self, context: &Context, related: u64) -> Option<(u64, u64)> {
        self.schema.rules.as_ref().map_or_else(
            || context.occurrence.zip(context.record),
            |(_, schema)| schema.relationship_origin(context, related),
        )
    }

    fn related_records(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        if !self.privileged() {
            return Err(CalcError::from_code(NO_PRIVILEGES).expect("nonzero"));
        }
        if record == NO_RECORD {
            return Ok(Vec::new());
        }
        self.related_set(occurrence, record, related)
    }

    fn custom_function(&self, id: u64) -> Option<Arc<om_calc::eval::CustomFunction>> {
        self.schema.rules.as_ref()?.1.custom_function(id)
    }

    fn found_record(&self, occurrence: u64, index: usize) -> Result<Option<u64>, CalcError> {
        let Some(table) = self.schema.table_of(occurrence) else {
            return Ok(None);
        };
        match &self.window {
            Some(w) if w.table == table => Ok(w.found.get(index).copied()),
            _ => Ok(self.all_records(table).get(index).copied()),
        }
    }

    /// Over the window's found set and its sort (file-metadata.md 5.3).
    fn summary(
        &self,
        context: &Context,
        summary: FieldId,
        break_field: FieldId,
    ) -> Result<Value, CalcError> {
        if !self.privileged() {
            return Err(CalcError::from_code(NO_PRIVILEGES).expect("nonzero"));
        }
        self.get_summary(context, summary, break_field)
    }

    fn base_table_name(&self, occurrence: u64) -> Option<&str> {
        let table = self.schema.table_of(occurrence)?;
        self.schema.catalog.table(table).map(|t| t.name.as_str())
    }

    /// The session's `Get` answers ([`get`]), as FileMaker Server's script
    /// engine gives them.
    fn get(&mut self, selector: u8, _context: &Context) -> Result<Value, CalcError> {
        self.get_answer(selector)
            .unwrap_or_else(|| Ok(self.get_value(selector)))
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        match scope {
            VarScope::Local => &mut self.locals,
            VarScope::Global => &mut self.global_vars,
        }
    }

    /// The Design functions and layout objects ([`design`]).
    fn call(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> Result<Value, CalcError> {
        self.host_call(function, args, context)
    }

    fn call_reporting(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> (Value, Option<CalcError>) {
        if let Some(variant) = Variant::of(&function) {
            return self.execute_sql(variant, args);
        }
        match self.call(function, args, context) {
            Ok(v) => (v, None),
            Err(e) => (Value::invalid(), Some(e)),
        }
    }
}

impl ServerHost {
    /// `Get ( selector )`: the session, its window and the server.
    fn get_value(&self, selector: u8) -> Value {
        use selector::*;
        let window = self.window.as_ref();
        let layout = window.and_then(|w| self.schema.layouts.get(w.layout));
        let record = window.and_then(Window::record);
        let open = self
            .record
            .as_ref()
            .filter(|r| window.is_some_and(|w| w.table == r.table) && Some(r.id) == record);
        let number = |n: usize| Value::number(n as i64);
        match selector {
            APPLICATION_VERSION => Value::text(format!("Server {}", env!("CARGO_PKG_VERSION"))),
            CURRENT_DATE => Value::Date(today()),
            CURRENT_TIME => Value::Time(now_time()),
            CURRENT_TIMESTAMP | CURRENT_HOST_TIMESTAMP => {
                match Timestamp::from_date_time(today(), now_time()) {
                    Some(stamp) => Value::Timestamp(stamp),
                    None => Value::invalid(),
                }
            }
            CURRENT_TIME_UTC_MILLISECONDS => Value::number((utc_micros() / 1000) as i64),
            // Get ( DocumentsPath ), Get ( TemporaryPath ) ([`transfer`]).
            73 | 89 => self.folder_path(selector),
            // Get ( OpenDataFileInfo ).
            123 => self.data_file_info(),
            CURRENT_TIME_UTC_MICROSECONDS => Value::number(utc_micros() as i64),
            FILE_NAME => Value::text(
                self.hosted
                    .name
                    .rsplit_once('.')
                    .map_or(self.hosted.name.as_str(), |(stem, _)| stem),
            ),
            FILE_PATH => Value::text(format!(
                "file:{}",
                std::fs::canonicalize(&self.hosted.path)
                    .unwrap_or_else(|_| self.hosted.path.clone())
                    .display()
            )),
            FILE_SIZE => {
                number(std::fs::metadata(&self.hosted.path).map_or(0, |m| m.len() as usize))
            }
            HOST_NAME => Value::text(&self.shared.identity.name),
            PERSISTENT_ID => Value::text(&self.shared.identity.server_id),
            USER_NAME | ACCOUNT_NAME => Value::text(&self.account),
            om_data::host::session::selector::ACCOUNT_TYPE
            | om_data::host::session::selector::ACCOUNT_PASSWORD_DAYS_REMAINING => self
                .authenticated_account_info()
                .answer(selector, today())
                .unwrap_or_else(Value::empty_text),
            SYSTEM_LANGUAGE | APPLICATION_LANGUAGE => Value::text("English"),
            // The file's locale record (file-metadata.md 2.4), as FileMaker
            // Server answers it.
            FILE_LOCALE_ELEMENTS => Value::text(self.locale.elements_json(true)),
            SYSTEM_PLATFORM => match std::env::consts::OS {
                "macos" => Value::number(1),
                "windows" => Value::number(-2),
                "linux" => Value::number(8),
                _ => Value::empty_text(),
            },
            DEVICE => match std::env::consts::OS {
                "macos" => Value::number(1),
                "windows" => Value::number(2),
                "linux" => Value::number(6),
                _ => Value::number(0),
            },
            APPLICATION_ARCHITECTURE => Value::text(match std::env::consts::ARCH {
                "aarch64" => "arm64",
                other => other,
            }),
            UUID => Value::text(self.uuid()),
            LAYOUT_COUNT => number(self.schema.layouts.len()),
            LAYOUT_NAME => Value::text(layout.map(|l| l.name.clone()).unwrap_or_default()),
            LAYOUT_NUMBER => match window.filter(|w| w.layout < self.schema.layouts.len()) {
                Some(w) => number(w.layout + 1),
                None => Value::empty_text(),
            },
            LAYOUT_TABLE_NAME => Value::text(
                window
                    .as_ref()
                    .and_then(|w| self.schema.names.occurrence_name(w.occurrence))
                    .unwrap_or_default(),
            ),
            WINDOW_MODE => match window.map(|w| w.mode) {
                Some(Mode::Find) => Value::number(1),
                Some(Mode::Preview) => Value::number(2),
                Some(Mode::Layout) => Value::number(4),
                _ => Value::number(0),
            },
            FOUND_COUNT => number(window.map_or(0, |w| w.found.len())),
            TOTAL_RECORD_COUNT => number(
                window
                    .as_ref()
                    .map_or(0, |w| self.all_records(w.table).len()),
            ),
            RECORD_NUMBER => match window {
                Some(w) if record.is_some() => number(w.current + 1),
                _ => Value::number(0),
            },
            RECORD_ID => record.map_or_else(Value::empty_text, |id| Value::number(id as i64)),
            RECORD_MODIFICATION_COUNT => match (window, record) {
                (Some(w), Some(id)) => {
                    let node = self.record_node(w.table, id);
                    number(modcount::serial_value(
                        node.as_ref().and_then(|n| n.child(&[modcount::MOD_COUNT])),
                    ) as usize)
                }
                _ => Value::empty_text(),
            },
            RECORD_OPEN_COUNT => number(usize::from(
                self.record.is_some() || self.find_request_open(),
            )),
            RECORD_OPEN_STATE => Value::number(match open {
                None if self.find_request_open() => 1,
                None => 0,
                Some(r) if r.new => 1,
                Some(_) => 2,
            }),
            MODIFIED_FIELDS => Value::text(
                open.map(|r| {
                    // The field being typed into counts once it is left.
                    let typed = self.focus.as_ref().filter(|f| f.typed).map(|f| f.field);
                    let changed: BTreeSet<u64> = r
                        .changed
                        .iter()
                        .copied()
                        .filter(|c| Some(*c) != typed)
                        .collect();
                    self.modified_fields(r.table, &changed, r.new).join("\r")
                })
                .unwrap_or_default(),
            ),
            SORT_STATE => number(window.map_or(0, |w| usize::from(w.sort.is_some()))),
            REQUEST_COUNT => match window.map(|w| w.mode) {
                Some(Mode::Find) => number(self.requests.len()),
                _ => number(self.request_count),
            },
            _ => Value::empty_text(),
        }
    }
}

impl ScriptHost for ServerHost {
    /// The window's current record; in Find mode, or without a current
    /// record, [`NO_RECORD`].
    fn calc_context(&mut self) -> Context {
        match &self.window {
            Some(w) => match w.record().filter(|_| w.mode != Mode::Find) {
                Some(record) => Context::record(w.occurrence, record),
                None => Context::record(w.occurrence, NO_RECORD),
            },
            None => Context::new(),
        }
    }

    fn current_file(&mut self) -> FileId {
        FILE
    }

    fn current_window(&mut self) -> Option<WindowId> {
        self.window.as_ref().map(|_| WINDOW)
    }

    fn script(&mut self, file: FileId, id: u64) -> Option<Arc<Script>> {
        if file != FILE {
            return None;
        }
        self.schema
            .scripts
            .iter()
            .find(|s| s.id == id && s.kind == ScriptKind::Script)
            .cloned()
    }

    fn script_named(&mut self, file: FileId, name: &str) -> Option<u64> {
        if file != FILE {
            return None;
        }
        let name = name.to_lowercase();
        self.schema
            .scripts
            .iter()
            .find(|s| s.kind == ScriptKind::Script && s.name.to_lowercase() == name)
            .map(|s| s.id)
    }

    /// The data source name of the file: its name without the extension
    /// (compared with case, as `source::script` is).
    fn file_named(&mut self, name: &str) -> Option<FileId> {
        let stem = self
            .hosted
            .name
            .rsplit_once('.')
            .map_or(self.hosted.name.as_str(), |(stem, _)| stem);
        (name == stem || name == self.hosted.name).then_some(FILE)
    }

    fn window_for_file(&mut self, file: FileId) -> HostResult<Option<WindowId>> {
        match (file, &self.window) {
            (FILE, Some(_)) => Ok(Some(WINDOW)),
            _ => Err(ErrorCode::INVALID.into()),
        }
    }

    fn window_mode(&mut self) -> Mode {
        self.window.as_ref().map_or(Mode::Browse, |w| w.mode)
    }

    /// The server's event log (script-runtime.md 4.9).
    fn log_error(&mut self, entry: &ErrorLogEntry) {
        self.log.push(&self.hosted.name, entry);
    }

    /// Pause/Resume Script waits out its duration on the server (an
    /// indefinite pause is 0.1 s there, which the runtime applies), in
    /// slices, ending early when the script is aborted
    /// (`SMScriptRuntime::OnIdle`, script-runtime.md 6.2).
    fn pause(&mut self, seconds: Option<f64>) -> HostResult<PauseEnd> {
        let seconds = seconds.unwrap_or(0.1);
        if !(seconds.is_finite() && seconds > 0.0) {
            return Ok(PauseEnd::Resumed);
        }
        let end = Instant::now() + Duration::from_secs_f64(seconds.min(1e9));
        loop {
            if self.control.aborted().is_some() {
                return Ok(PauseEnd::Canceled);
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(PauseEnd::Resumed);
            }
            std::thread::sleep(left.min(PAUSE_SLICE));
        }
    }

    /// The administrator, a file close, the server's shutdown or a limit
    /// aborted the script (`IsServerAbort`, script-runtime.md 2.3, 6.1).
    fn abort_requested(&mut self) -> bool {
        self.control.aborted().is_some()
    }

    /// Each step sees the schema as it is when the step starts.
    fn before_step(&mut self) {
        self.refresh_schema();
    }

    /// An account with [Full Access] may run every script; any other may
    /// run only scripts set to run with full access privileges, whose
    /// steps then have full access (script-runtime.md 2.2, 9.1). The
    /// script privileges of other privilege sets are not decoded.
    fn script_access(&mut self, file: FileId, script: u64) -> ScriptAccess {
        ScriptAccess {
            executable: self.privileges == Privileges::FullAccess,
            full_access: file == FILE && self.schema.full_access.contains(&script),
        }
    }

    fn set_full_access(&mut self, _file: FileId, on: bool) {
        self.elevated = on;
    }
}

impl Navigation for ServerHost {
    fn go_to_layout(&mut self, layout: &LayoutTarget, _animation: Option<u32>) -> HostResult {
        let layouts = &self.schema.layouts;
        let index = match layout {
            LayoutTarget::Original => return Ok(()),
            LayoutTarget::Key(id) => layouts.iter().position(|l| l.id == *id),
            LayoutTarget::Name(name) => {
                layouts.iter().position(|l| l.name == *name).or_else(|| {
                    let name = name.to_lowercase();
                    layouts.iter().position(|l| l.name.to_lowercase() == name)
                })
            }
            LayoutTarget::Number(n) => usize::try_from(*n - 1).ok().filter(|i| *i < layouts.len()),
        }
        .filter(|i| layouts[*i].occurrence.is_some())
        .ok_or(ErrorCode::LAYOUT_MISSING)?;
        self.leave()?;
        let mut next = self.window_on(index).ok_or(ErrorCode::LAYOUT_MISSING)?;
        if let Some(old) = &self.window {
            next.mode = old.mode;
            if old.table == next.table {
                // The same table keeps its found set, sort and record.
                next.found.clone_from(&old.found);
                next.current = old.current;
                next.sort.clone_from(&old.sort);
            }
        }
        self.window = Some(next);
        Ok(())
    }

    fn go_to_record(&mut self, movement: RecordMove, _with_dialog: bool) -> HostResult<bool> {
        self.leave()?;
        let count = match self.window()?.mode {
            Mode::Find => self.requests.len(),
            _ => self.window()?.found.len(),
        };
        let window = self.window_mut()?;
        if count == 0 {
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        let last = count - 1;
        window.current = match movement {
            RecordMove::First => 0,
            RecordMove::Last => last,
            RecordMove::Previous if window.current == 0 => return Ok(false),
            RecordMove::Previous => window.current - 1,
            RecordMove::Next if window.current >= last => return Ok(false),
            RecordMove::Next => window.current + 1,
            RecordMove::Number(n) if n < 1 => return Err(ErrorCode::RECORD_MISSING.into()),
            RecordMove::Number(n) => (n as usize - 1).min(last),
        };
        Ok(true)
    }

    /// Go to Field: the field gets the focus; with none the field is left,
    /// which commits the record (s33).
    fn go_to_field(&mut self, field: Option<FieldRef>, _select_or_perform: bool) -> HostResult {
        match field {
            Some(field) => {
                self.target(field)?;
                self.focus_field(field.occurrence, field.field);
                Ok(())
            }
            None if self.focus.is_some() => self.leave(),
            None => Ok(()),
        }
    }

    fn go_to_related_record(&mut self, request: &RelatedRecords<'_>) -> HostResult {
        self.go_to_related(request)
    }

    fn go_to_list_of_records(
        &mut self,
        ids: &str,
        layout: Option<&LayoutTarget>,
        _animation: Option<u32>,
        new_window: bool,
    ) -> HostResult {
        if new_window {
            return Err(HostError::Unsupported);
        }
        if let Some(layout) = layout {
            self.go_to_layout(layout, None)?;
        }
        self.leave()?;
        let table = self.window()?.table;
        let existing: BTreeSet<u64> = self.all_records(table).into_iter().collect();
        let found: Vec<u64> = ids
            .split(['\r', '\n'])
            .filter_map(|id| id.trim().parse().ok())
            .filter(|id| existing.contains(id))
            .collect();
        let window = self.window_mut()?;
        window.mode = Mode::Browse;
        window.found = found;
        window.current = 0;
        window.sort = None;
        if window.found.is_empty() {
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        Ok(())
    }

    fn enter_mode(&mut self, mode: Mode, _pause: bool, requests: Option<&OwnedNode>) -> HostResult {
        // Enter Find Mode [Restore] starts with its stored requests
        // (script-steps.md 11.2).
        let stored = match requests {
            Some(node) if mode == Mode::Find => Some(self.stored_requests(node)?),
            _ => None,
        };
        self.leave()?;
        match mode {
            Mode::Browse => {
                let window = self.window_mut()?;
                let was_find = window.mode == Mode::Find;
                if was_find {
                    window.current = 0;
                }
                window.mode = Mode::Browse;
                // `Get ( RequestCount )` keeps Find mode's requests (s31,
                // s33).
                if was_find {
                    self.request_count = self.requests.len();
                }
                self.requests.clear();
            }
            Mode::Find => {
                self.requests = stored.unwrap_or_else(|| vec![Request::default()]);
                let window = self.window_mut()?;
                if window.mode != Mode::Find {
                    window.browse_current = window.current;
                }
                window.mode = Mode::Find;
                window.current = 0;
                self.focus_first_field();
            }
            Mode::Preview | Mode::Layout => return Err(HostError::Unsupported),
        }
        Ok(())
    }
}

impl Records for ServerHost {
    fn new_record(&mut self) -> HostResult {
        if self.window()?.mode == Mode::Find {
            self.requests.push(Request::default());
            let last = self.requests.len() - 1;
            self.window_mut()?.current = last;
            self.request_open = true;
            if self.focus.is_none() {
                self.focus_first_field();
            }
            return Ok(());
        }
        self.leave()?;
        let table = self.window()?.table;
        self.require_privileges()?;
        // Rules the server cannot enforce refuse the record before an id
        // is reserved.
        let plan = self.plan(table)?;
        let reply = writes::create_record(
            &self.hosted,
            self.user,
            &self.holder,
            &CreateRecord {
                table: vec![uint_segment(table)],
                count: 1,
                txn: self.user.txn,
            },
            writes::UNCAPPED,
            false,
        );
        let id = match reply.status {
            0 => reply
                .record
                .first()
                .and_then(|key| serial::decode(key))
                .ok_or(ErrorCode::UNKNOWN)?,
            106 => return Err(TABLE_MISSING.into()),
            _ => return Err(RECORD_IN_USE.into()),
        };
        // Its serial numbers generated on creation are taken now, as
        // FileMaker Pro takes them right after op71 (write-path.md 1.1): the
        // script reads them before the commit.
        let skip_auto_enter = self
            .transaction
            .as_ref()
            .is_some_and(|t| t.options.skip_auto_enter);
        let on_creation = plan
            .serials_for(WriteMode::Create, Entry::script(false, skip_auto_enter))
            .into_iter()
            .filter(|serial| !serial.on_commit);
        let serials = self.reserve_serials(table, on_creation)?;
        let mut node = OwnedNode::list(serial::encode(id), Vec::new());
        for (field, text) in &serials {
            node.set_child(values::cell(&uint_segment(*field), text));
        }
        self.record = Some(OpenRecord {
            table,
            id,
            node,
            new: true,
            changed: BTreeSet::new(),
            ruled: None,
            serials,
            context: Box::new(self.step_context()),
        });
        let window = self.window_mut()?;
        window.found.push(id);
        window.current = window.found.len() - 1;
        // The record joins the end of the found set, and an unsorted found
        // set's position is what its auto-entry reads as Get (
        // RecordNumber ): n + 1 over the n records found before, wherever
        // the current record was, also when none were and for a duplicate
        // (s46, FileMaker Server's engine). A sorted found set's is
        // unmeasured and stays unanswered (#298).
        let number = (window.sort.is_none()).then_some(window.found.len() as u64);
        if let Some(record) = &mut self.record {
            record.context.record_number = number;
        }
        // The first field of the tab order gets the focus (s31, s33).
        self.focus_first_field();
        Ok(())
    }

    fn duplicate_record(&mut self) -> HostResult {
        let window = self.window()?;
        if window.mode == Mode::Find {
            let copy = self
                .requests
                .get(window.current)
                .cloned()
                .ok_or(ErrorCode::RECORD_MISSING)?;
            self.requests.push(copy);
            let last = self.requests.len() - 1;
            self.window_mut()?.current = last;
            return Ok(());
        }
        let (table, id) = (
            window.table,
            window.record().ok_or(ErrorCode::RECORD_MISSING)?,
        );
        let source = self
            .record_node(table, id)
            .ok_or(ErrorCode::RECORD_MISSING)?;
        self.new_record()?;
        // The copied values of normal fields are the new record's entries;
        // the rules compute its calculations and creation values again
        // when it is committed. A serial number is not copied: the copy
        // takes the next one (`duplicate-record-request.html`).
        let fields = self.schema.catalog.table(table);
        let normal = |kind: &dyn Fn(&Field) -> bool| {
            fields.map_or_else(BTreeSet::new, |t| {
                t.fields
                    .iter()
                    .filter(|f| f.kind == FieldKind::Normal && kind(f))
                    .map(|f| f.id)
                    .collect::<BTreeSet<u64>>()
            })
        };
        let entered = normal(&|f: &Field| f.data_type != DataType::Container);
        let serials =
            normal(&|f: &Field| f.decoded_options().is_some_and(|o| o.auto_entry_serial()));
        // An auto-entered calculation that replaces an existing value is
        // entered again, as FileMaker Server's engine entered the copy's
        // own Get ( RecordNumber ) into it (s46): not copied.
        let replacing = normal(&|f: &Field| {
            f.decoded_options()
                .is_some_and(|o| o.calculation_replaces_existing())
        });
        let record = self.record.as_mut().expect("the new record");
        for cell in source.children {
            if cell.segment == [modcount::MOD_COUNT] {
                continue;
            }
            if let Some(field) = om_hbam::slot::be_uint(&cell.segment) {
                if serials.contains(&field) || replacing.contains(&field) {
                    continue;
                }
                if entered.contains(&field) {
                    record.changed_cell(field);
                }
            }
            record.node.set_child(cell);
        }
        Ok(())
    }

    fn delete_record(&mut self, _with_dialog: bool) -> HostResult {
        let window = self.window()?;
        if window.mode == Mode::Find {
            let current = window.current;
            if self.requests.len() <= 1 {
                return Err(HostError::Unsupported);
            }
            self.requests.remove(current);
            let last = self.requests.len() - 1;
            self.window_mut()?.current = current.min(last);
            return Ok(());
        }
        let id = window.record().ok_or(ErrorCode::RECORD_MISSING)?;
        self.delete(&[id], false)
    }

    fn delete_all_records(&mut self, _with_dialog: bool) -> HostResult {
        let found = self.window()?.found.clone();
        self.delete(&found, true)
    }

    /// Revert Record: the open record's changes are dropped. Inside a
    /// transaction they go back to what the transaction had (an edit made
    /// before the record was reopened stays).
    fn revert_record(&mut self, _with_dialog: bool) -> HostResult {
        self.focus = None;
        let Some(transaction) = &self.transaction else {
            self.drop_open();
            return Ok(());
        };
        if let Some(record) = self.record.take() {
            // The record is again what the transaction holds; a new one the
            // transaction does not hold is gone. Locks last until the
            // transaction ends.
            let held = transaction.changes.contains_key(&(record.table, record.id));
            if record.new && !held {
                if let Some(window) = self.window.as_mut() {
                    window.found.retain(|r| *r != record.id);
                    window.current = window.current.min(window.found.len().saturating_sub(1));
                }
            }
        }
        Ok(())
    }

    fn commit_records(&mut self, options: CommitOptions) -> HostResult {
        self.focus = None;
        self.commit_open(Entry::script(options.skip_validation, false))
    }

    fn open_record(&mut self) -> HostResult {
        self.open_current().map(|_| ())
    }

    fn truncate_table(&mut self, table: Option<u64>, _with_dialog: bool) -> HostResult {
        self.require_privileges()?;
        if table.is_some_and(|t| Some(t) != self.window.as_ref().map(|w| w.table)) {
            // Another table than the window's: not supported yet.
            return Err(HostError::Unsupported);
        }
        let all = self.all_records(self.window()?.table);
        self.delete(&all, true)
    }

    fn begin_transaction(&mut self, options: TransactionOptions) -> HostResult {
        self.leave()?;
        self.transaction = Some(Transaction::new(options));
        Ok(())
    }

    /// Commit Transaction: the open record joins the transaction; every
    /// record it changed gets its table's rules (auto-enter at commit,
    /// `open-transaction.html`), then all are written as one commit. A
    /// record the rules refuse reverts the whole transaction and the step
    /// fails with their error.
    fn commit_transaction(&mut self, skip_validation: bool) -> HostResult {
        if let Some(record) = self.record.take() {
            let changed = record.new || !record.changed.is_empty();
            match self.transaction.as_mut() {
                Some(transaction) if changed => {
                    transaction.insert((record.table, record.id), Change::Write(record));
                }
                _ => self.unlock_record(record.table, record.id),
            }
        }
        let Some(transaction) = self.transaction.as_ref() else {
            return Ok(());
        };
        let options = transaction.options;
        let entry = Entry::script(
            skip_validation || options.skip_validation,
            options.skip_auto_enter,
        );
        let keys: Vec<(u64, u64)> = transaction.changes.keys().copied().collect();
        for key in keys {
            let Some(transaction) = self.transaction.as_mut() else {
                break;
            };
            if !matches!(transaction.changes.get(&key), Some(Change::Write(_))) {
                continue;
            }
            let Some(Change::Write(mut record)) = transaction.remove(&key) else {
                continue;
            };
            let result = self.apply_rules(&mut record, entry);
            if let Some(transaction) = self.transaction.as_mut() {
                transaction.insert(key, Change::Write(record));
            }
            if let Err(e) = result {
                self.discard_transaction();
                return Err(e);
            }
        }
        let changes: Vec<((u64, u64), Change)> = self
            .transaction
            .take()
            .map(|t| t.changes.into_iter().collect())
            .unwrap_or_default();
        let (created, deleted) = created_and_deleted(&changes);
        let result = if changes.is_empty() {
            Ok(())
        } else {
            self.write(changes)
        };
        if result.is_err() {
            self.undo(created, deleted);
            return result;
        }
        self.hosted.lock_table().release_txn(self.user);
        Ok(())
    }

    fn revert_transaction(&mut self) -> HostResult {
        self.discard_transaction();
        Ok(())
    }
}

impl FieldSteps for ServerHost {
    fn export_field_contents(
        &mut self,
        field: Option<FieldRef>,
        paths: &[String],
        create_folders: bool,
    ) -> HostResult {
        self.export_contents(field, paths, create_folders)
    }

    fn set_field(&mut self, field: FieldRef, value: Value) -> HostResult {
        self.store_value(field, value)
    }

    fn set_field_by_name(&mut self, name: &str, value: Value) -> HostResult {
        let (occurrence, field) = self.field_named(name).ok_or(ErrorCode::FIELD_MISSING)?;
        self.store_value(
            FieldRef {
                occurrence,
                field,
                repetition: 1,
            },
            value,
        )
    }

    fn field_value_of(&mut self, field: FieldRef) -> HostResult<Value> {
        self.value_of(field)
    }

    /// Insert steps: into their target field, else the field with the
    /// focus.
    fn insert(&mut self, target: InsertTarget, value: Value) -> HostResult {
        let focused = self.focus.as_ref().map(|f| FieldRef {
            occurrence: f.occurrence,
            field: f.field,
            repetition: 1,
        });
        let field = target.field.or(focused).ok_or(HostError::Unsupported)?;
        // Into the focused field this is typing (s33).
        if let Some(focus) = self
            .focus
            .as_mut()
            .filter(|f| f.occurrence == field.occurrence && f.field == field.field)
        {
            focus.typed = true;
        }
        // Container data in a container field is not text to insert
        // into: it replaces the contents, as Set Field puts it
        // ([`containers`]).
        let into_container = matches!(value, Value::Container(_))
            && self.target(field)?.1.data_type == DataType::Container;
        let value = if target.select_all || into_container {
            value
        } else {
            let old = self.value_of(field).unwrap_or_else(|_| Value::empty_text());
            let cx = self.cx();
            let mut text = old.to_text(&cx).into_string();
            text.push_str(value.to_text(&cx).as_str());
            Value::text(text)
        };
        self.store_value(field, value)
    }

    /// Set Next Serial Value: written to disk before it counts, as a
    /// record commit is. Not once the script is aborted: the step's
    /// calculation may have been stopped, leaving `?`.
    fn set_next_serial_value(&mut self, field: FieldRef, value: Value) -> HostResult {
        self.require_privileges()?;
        if self.aborted() {
            return Err(ErrorCode::CANCELED.into());
        }
        let (table, _) = self.target(field)?;
        let text = value.to_text(&self.cx()).into_string();
        let mut store = self.hosted.write_store();
        let mut journal = store.begin_commit();
        store
            .set_serial_in(
                &mut journal,
                &uint_segment(table),
                &uint_segment(field.field),
                &text,
            )
            .ok_or(ErrorCode::FIELD_MISSING)?;
        self.hosted
            .save_commit(&mut store, journal)
            .map_err(|e| HostError::Error(ErrorCode(i32::from(crate::hosting::save_error(&e)))))
    }
}

impl FoundSets for ServerHost {
    fn show_all_records(&mut self) -> HostResult {
        self.leave()?;
        let table = self.window()?.table;
        let all = self.all_records(table);
        let window = self.window_mut()?;
        window.mode = Mode::Browse;
        window.found = all;
        window.current = 0;
        window.sort = None;
        self.requests.clear();
        Ok(())
    }

    fn show_omitted_only(&mut self) -> HostResult {
        self.leave()?;
        let table = self.window()?.table;
        let all = self.all_records(table);
        let window = self.window_mut()?;
        let found: BTreeSet<u64> = window.found.iter().copied().collect();
        window.found = all.into_iter().filter(|r| !found.contains(r)).collect();
        window.current = 0;
        window.sort = None;
        Ok(())
    }

    fn omit_records(&mut self, count: i64, _with_dialog: bool) -> HostResult {
        if self.window()?.mode == Mode::Find {
            // Omit in Find mode marks the request as an omit request.
            let current = self.window()?.current;
            let request = self
                .requests
                .get_mut(current)
                .ok_or(ErrorCode::RECORD_MISSING)?;
            request.omit = !request.omit;
            return Ok(());
        }
        self.leave()?;
        let window = self.window_mut()?;
        if window.found.is_empty() {
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        let start = window.current.min(window.found.len());
        let end = (start + count.max(1) as usize).min(window.found.len());
        window.found.drain(start..end);
        window.current = window.current.min(window.found.len().saturating_sub(1));
        Ok(())
    }

    /// The found set back in creation order.
    fn unsort_records(&mut self) -> HostResult {
        self.leave()?;
        let window = self.window_mut()?;
        let current = window.record();
        window.found.sort_unstable();
        window.current = current
            .and_then(|id| window.found.iter().position(|r| *r == id))
            .unwrap_or(0);
        window.sort = None;
        Ok(())
    }

    fn modify_last_find(&mut self) -> HostResult {
        self.leave()?;
        self.requests = if self.last_find.is_empty() {
            vec![Request::default()]
        } else {
            self.last_find.clone()
        };
        let window = self.window_mut()?;
        if window.mode != Mode::Find {
            window.browse_current = window.current;
        }
        window.mode = Mode::Find;
        window.current = 0;
        Ok(())
    }

    /// Perform Find, Constrain and Extend Found Set, with the stored
    /// requests or the window's (script-steps.md 11.2.4): Constrain finds
    /// within the found set, Perform Find and Extend within all records,
    /// Extend adding what it finds to the found set.
    fn perform_find(&mut self, kind: FindKind, requests: Option<&OwnedNode>) -> HostResult {
        // A find reads field values.
        self.require_privileges()?;
        let stored = requests
            .map(|node| self.stored_requests(node))
            .transpose()?;
        self.leave()?;
        let in_find_mode = self.window()?.mode == Mode::Find;
        let requests = match stored {
            Some(stored) => stored,
            None if in_find_mode => std::mem::take(&mut self.requests),
            None => self.last_find.clone(),
        };
        self.request_count = requests.len();
        let window = self.window()?;
        let current: RidList = window.found.iter().copied().collect();
        let all: RidList = self.all_records(window.table).into_iter().collect();
        let master = if kind == FindKind::Constrain {
            &current
        } else {
            &all
        };
        let matched = match self.find_within(&requests, master) {
            Ok(found) => found,
            Err(e) if e.code() == NO_CRITERIA.0 => {
                // Without criteria FileMaker's engine is back in Browse
                // mode, the found set and current record as they were
                // (script-runtime.md 13.19, 13.21).
                self.requests.clear();
                let window = self.window_mut()?;
                if window.mode == Mode::Find {
                    window.current = window
                        .browse_current
                        .min(window.found.len().saturating_sub(1));
                }
                window.mode = Mode::Browse;
                return Err(NO_CRITERIA.into());
            }
            Err(e) => {
                if in_find_mode && self.requests.is_empty() {
                    self.requests = requests;
                }
                return Err(ErrorCode(e.code()).into());
            }
        };
        self.last_find = requests;
        self.requests.clear();
        let window = self.window_mut()?;
        let found = match kind {
            FindKind::Replace | FindKind::Constrain => matched,
            FindKind::Extend => current.or(&matched),
        };
        window.found = found.iter().collect();
        window.current = 0;
        window.mode = Mode::Browse;
        window.sort = None;
        if window.found.is_empty() {
            return Err(NO_RECORDS_MATCH.into());
        }
        Ok(())
    }

    /// Sort Records with its stored order (script-steps.md 11.1); without
    /// one the server has no dialog to ask with (3).
    fn sort_records(&mut self, order: Option<&OwnedNode>, _with_dialog: bool) -> HostResult {
        let node = order.ok_or(HostError::Unsupported)?;
        let spec = SortSpec::from_node(node).ok_or(ErrorCode::INVALID)?;
        self.sort_window(&spec)
    }

    fn sort_by_field(&mut self, field: Option<FieldRef>, direction: u16) -> HostResult {
        let field = field.ok_or(ErrorCode::FIELD_MISSING)?;
        self.sort_window_by_field(field, direction)
    }
}

impl Windows for ServerHost {
    /// Nothing is drawn on the server.
    fn refresh_window(&mut self, _flush_joins: bool, _flush_external: bool) -> HostResult {
        Ok(())
    }
}

/// A session dropped without [`ServerHost::close`] (its thread unwinds from
/// a panic) still gives up its locks: nothing of it was committed, and
/// other users must not find its records held until the server restarts.
/// Its temporary folder goes too, with the files its steps wrote there.
impl Drop for ServerHost {
    fn drop(&mut self) {
        self.release_locks();
        self.data_files.close_all();
        self.remove_temporary_folder();
    }
}

impl Dialogs for ServerHost {}
impl Files for ServerHost {
    fn import_records(&mut self, call: &om_scripts::runtime::TransferCall<'_>) -> HostResult {
        self.import_step(call)
    }

    fn export_records(&mut self, call: &om_scripts::runtime::TransferCall<'_>) -> HostResult {
        self.export_step(call)
    }

    // The data file steps, beneath the Documents and temporary folders
    // ([`files`]).

    fn file_exists(&mut self, paths: &[String]) -> HostResult<bool> {
        Ok(self.data_file_exists(paths))
    }

    fn file_size(&mut self, paths: &[String]) -> HostResult<i64> {
        Ok(self.data_file_size(paths)?)
    }

    fn create_data_file(&mut self, paths: &[String], create_folders: bool) -> HostResult {
        Ok(self.create_data_file_at(paths, create_folders)?)
    }

    fn open_data_file(&mut self, paths: &[String]) -> HostResult<i64> {
        Ok(self.open_data_file_at(paths)?)
    }

    fn write_data_file(
        &mut self,
        id: i64,
        data: &Value,
        encoding: om_scripts::runtime::Encoding,
        line_feed: bool,
    ) -> HostResult {
        Ok(self.write_data_file_at(id, data, encoding, line_feed)?)
    }

    fn read_data_file(
        &mut self,
        id: i64,
        amount: Option<i64>,
        encoding: om_scripts::runtime::Encoding,
    ) -> HostResult<Value> {
        Ok(self.data_files.read(id, amount, encoding)?)
    }

    fn data_file_position(&mut self, id: i64) -> HostResult<i64> {
        Ok(self.data_files.position(id)?)
    }

    fn set_data_file_position(&mut self, id: i64, position: i64) -> HostResult {
        Ok(self.data_files.set_position(id, position)?)
    }

    fn close_data_file(&mut self, id: i64) -> HostResult {
        Ok(self.data_files.close(id)?)
    }

    fn delete_file(&mut self, paths: &[String]) -> HostResult {
        Ok(self.delete_data_file(paths)?)
    }

    fn rename_file(&mut self, paths: &[String], new_name: &str) -> HostResult {
        Ok(self.rename_data_file(paths, new_name)?)
    }
}
impl Network for ServerHost {
    fn fetch_url(
        &mut self,
        request: &om_scripts::runtime::UrlRequest,
    ) -> HostResult<om_scripts::runtime::UrlResponse> {
        self.fetch_url_step(request)
    }
}
impl Accounts for ServerHost {}
impl Platform for ServerHost {}
