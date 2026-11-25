//! A hosted file's contents while it is hosted: its whole key tree in memory,
//! the source of every download and query and the target of every write
//! (spec `docs/re/write-path.md` 5).
//!
//! Record writes keep the table's bookkeeping current as FileMaker Server
//! does: the record-id list `[T, 0d]`, the last issued record id `[T, 00]`,
//! and every value and word index `[T, 01, F, ...]`, whose keys are computed
//! from the stored values with `om-collate`, cut as FileMaker cuts them
//! (query-program.md 7.1). Schema writes (`docs/re/
//! schema-path.md`) store what the client uploads and re-read the schema;
//! op73 then does the field work the client listed and recomputes the
//! stored calculations the change made stale ([`crate::recalc`]).
//!
//! The tree only takes what the file can hold (`om_hbam::writer`'s limits),
//! checked against the tree as it is: a write that would go beneath a node
//! holding a value (making a list of the value) or leave a key or path the
//! file cannot hold is refused before it changes anything, with the refusal
//! its operation reports, and a commit with such a write is taken back
//! whole. After a failed save only field indexes are ever taken out
//! ([`Store::drop_unwritable_indexes`]); a file that loads with content
//! beyond the limits is served read-only.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::{error, info, warn};
use om_calc::Locale;
use om_collate::DateOrder;
use om_data::rules::recalc::{ContainerResult, Recalculation};
use om_data::rules::RecordEnvironment;
use om_hbam::slot::{be_uint, uint_segment};
use om_hbam::writer::{
    holds_value, put_refusals, unwritable, unwritable_in_tree, value_on_path, Unwritable,
};
use om_hbam::{serial, Catalog, DataType, HbamFile, KeyTree, OwnedNode, RidList};
use om_query::keys::{record_value_entries, word_entries, Reading, TextKeys};
use om_query::Write;

use om_data::containers::{self, MemberInfo};

use crate::anchored::{FileAt, FileId, Folder};
use crate::derived::{
    self, cell_text, counts_key, field_texts, Derived, CATALOG_INDEX, ENGLISH, LAST_MEMBER_ID,
    LOCALE,
};
use crate::engine::{TreeView, Version, Waiting};
use crate::filelock::FileLock;
use crate::library::{self, BinaryData, Stored, LIBRARY};
use crate::modcount::{self, MOD_COUNT};
use crate::recalc::{self, TableBasis};

/// Keys under a base table `T` (query-program.md 7.1, write-path.md 5).
pub(crate) mod keys {
    pub const LAST_RECORD_ID: u8 = 0x00;
    pub const INDEXES: u8 = 0x01;
    pub const FIELDS: u8 = 0x03;
    pub const RECORDS: u8 = 0x05;
    /// A table's global values `[T, 08, F]` (calc-values.md, "Global
    /// values").
    pub const GLOBALS: u8 = 0x08;
    pub const RECORD_IDS: u8 = 0x0d;
    /// Under `[T, 03]`: the field definitions, then a field's next serial.
    pub const FIELD_DEFINITIONS: u8 = 0x05;
    pub const SERIAL_VALUE: u8 = 0x0c;
    /// Under `[T, 01, F]`: the value and word indexes and their counts.
    pub const VALUE_INDEX: u8 = 0x00;
    pub const WORD_INDEX: u8 = 0x01;
    pub const VALUE_COUNTS: u8 = 0x10;
    pub const WORD_COUNTS: u8 = 0x11;
    /// Under `[T, 03]`: the field catalog's index; under that, the deferred
    /// field work (schema-path.md 7.2).
    pub const FIELD_CATALOG_INDEX: u8 = 0x01;
    pub const DEFERRED: u8 = 0xfd;
}

/// Deferred field work bits (schema-path.md 7.2).
mod deferred {
    /// The field was deleted, or its storage changed: drop its values.
    pub const DELETE: u32 = 0x1;
    /// The old field was global: the work applies to the table's global
    /// value instead of its records.
    pub const GLOBAL: u32 = 0x2;
    /// The repetition count was reduced: delete the repetitions above it
    /// (with `0x8` on a container, releasing them).
    pub const FEWER: u32 = 0x4;
    /// The old field was a container: release its library members. Without
    /// `0x1` or `0x4` it is a type change from container, which deletes
    /// every repetition (schema-path.md 7.2.1).
    pub const CONTAINER: u32 = 0x8;
    /// (Re)build the field's indexes.
    pub const BUILD_INDEX: u32 = 0x10;
    /// The data type, storage or repetitions changed.
    pub const CONVERT: u32 = 0x40;
    /// Delete the field's indexes.
    pub const DROP_INDEX: u32 = 0x20;
    /// A normal field changed to date: rewrite each stored value as a date
    /// (schema-path.md 7.2.1).
    pub const TO_DATE: u32 = 0x400;
    /// A normal field changed to timestamp: rewrite each stored value as a
    /// timestamp (7.2.1).
    pub const TO_TIMESTAMP: u32 = 0x800;
    pub const KNOWN: u32 = 0x1fff;
    pub const REMOVES_VALUES: u32 = DELETE | FEWER | CONTAINER;
    // `0x40` alone changes no value of the field's own; its dependents are
    // recalculated ([`crate::recalc`]). `0x1000` (fewer repetitions) does
    // nothing by itself.
}

/// Field option bits of the definition's `mOptionSet` (schema-path.md 5.5).
const VALUE_INDEX_OPTION: u32 = 0x8000_0000;
const WORD_INDEX_OPTION: u32 = 0x2000_0000;

/// `node` as a key tree [`om_hbam::Node`] borrowing its keys and values:
/// what decoders that read a `Node` take (`Script::decode`, the security
/// catalog's). It takes a place for each node and nothing for keys or
/// values, so less than `node`'s own size (`authoring::memory`).
/// Recursive: a definition read from a request, or from the file, is at
/// most 17 keys deep (`om_hbam::slot::MAX_DEPTH`).
pub(crate) fn view(node: &OwnedNode) -> om_hbam::Node<'_> {
    om_hbam::Node {
        segment: om_hbam::KeySegment(&node.segment),
        value: node.value.as_deref(),
        push: node.push,
        children: node.children.iter().map(view).collect(),
    }
}

/// A hosted file's key tree and schema.
pub struct Store {
    root: OwnedNode,
    /// The version of the tree: how many times the store was taken for
    /// writing (`engine::View::version`).
    version: Version,
    /// The state readers need besides the tree: the decoded catalog, the
    /// locale and date order, the file's name and whether it is served to a
    /// desktop ([`Derived`]): computed when the file is loaded, and the
    /// catalog again after each change of the schema
    /// ([`Store::reread_catalog`]).
    derived: Arc<Derived>,
    /// The file's name, as calculations see it (`Get ( FileName )`).
    name: String,
    /// Per table, the field definitions its stored values were computed
    /// with ([`recalc`]); a table without one has every stored calculation
    /// recomputed by its next op73.
    basis: HashMap<Vec<u8>, TableBasis>,
    /// The file's page 0, kept for writing the file back; its SCN advances
    /// with each save.
    superblock: Vec<u8>,
    /// Whether the tree has changes the file on disk lacks.
    dirty: bool,
    /// How many saves wrote the file since it was loaded
    /// ([`Store::saves`]).
    saves: u64,
    /// A test makes every commit's save fail, as storage refusing it would,
    /// while other saves work ([`crate::ServerHandle::failing_commit_saves`]).
    /// Only in debug builds.
    #[cfg(debug_assertions)]
    pub(crate) fail_commit_saves: bool,
    /// A test makes every save fail at one step ([`SaveFault`],
    /// [`crate::ServerHandle::failing_saves`]). Only in debug builds.
    #[cfg(debug_assertions)]
    pub(crate) save_fault: Option<SaveFault>,
    /// Why the file takes no more changes: a save renamed its new copy over
    /// the file and could not flush the folder, so whether the file on disk
    /// keeps the change is unknown ([`SaveError::Unknown`]). Every later
    /// save is refused; hosting the file again clears it.
    unknown: Option<String>,
    /// Counts the schema changes (catalogs replaced, op73's field work):
    /// what was read from the schema before a change is stale after it
    /// ([`Store::schema_generation`]).
    generation: u64,
    /// The schema generation the derived state's accounts were decoded at
    /// ([`Store::follow_accounts`]).
    accounts_at: u64,
    /// What reads left waiting for a write ([`Waiting`]): the member ids
    /// and modification counts GenerateMemberKey issued per catalog while
    /// the tree was shared for reading, until
    /// [`Store::take_issued_member_keys`] takes them to be written into the
    /// catalogs (schema-path.md 3.4), and the indexes (table, field, kind)
    /// queries built on demand, until [`Store::take_indexes_on_demand`]
    /// takes them to be stored.
    waiting: Waiting,
    /// Whether each index a write checked holds the keys the current rules
    /// give its records ([`Store::index_agrees`]), at schema generation
    /// `verified.0`: a rollback or a schema change, which raise the
    /// generation, empties it.
    verified: (u64, BTreeMap<IndexId, bool>),
    /// The lock this process holds on the file ([`crate::filelock`]), for
    /// a file hosted privately for a local client; each save moves it to
    /// the file it writes.
    lock: Option<FileLock>,
    /// The device and inode of the file on disk the store was read from or
    /// last saved as, where it has them ([`Store::set_file`]): a save
    /// without a lock writes only where the path still leads to it (#468).
    file: Option<FileId>,
    /// Why the file may not be written, if it may not: saves fail, their
    /// changes stay in memory ([`Store::set_read_only`]).
    read_only: Option<ReadOnlyCause>,
}

/// Why a hosted file may not be written ([`Store::set_read_only`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOnlyCause {
    /// The desktop's host opened it so: it may not be written there, it
    /// has other names, it cannot be locked, or something is at its save's
    /// copy name (`crate::local::ReadOnly`).
    Opened,
    /// It holds content the writer cannot write back.
    Unwritable,
    /// Its deferred field work cannot be done, or could not be saved, and
    /// is kept (`Store::recover`, op73).
    DeferredWork,
}

/// An index: its table, field and kind (value 0, word 1).
pub type IndexId = (Vec<u8>, Vec<u8>, u8);

/// The file as a store last loaded or saved it ([`Store::saved_file`]),
/// to read by position and nothing else: the descriptor inside is a clone
/// of the store's own, which is open for writing, and no way to it or to
/// a write leaves this type.
#[cfg(target_os = "linux")]
pub struct SavedFile(std::fs::File);

#[cfg(target_os = "linux")]
impl SavedFile {
    /// Read into `buf` from byte `offset` of the file, leaving the offset
    /// the descriptor shares alone (`pread`).
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(&self.0, buf, offset)
    }
}

/// What became of the member keys a request issued
/// ([`Store::issued_keys`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssuedKeysAre<'a> {
    /// Each is in its catalog's index.
    Stored,
    /// One of them is not in its catalog's index: no save stored it.
    NotStored,
    /// The file takes no more changes, for this reason: whether they are
    /// on disk is unknown.
    Unknown(&'a str),
}

impl IssuedKeysAre<'_> {
    /// The FileMaker error an op86 request that issued these keys answers
    /// with its error slot, or `None` for the program's own answer:
    /// `draft_failed`, the error of its own draft when that was not
    /// stored; `own_writes`, whether the program had catalog writes of its
    /// own besides the keys. The file taking no more changes answers 800
    /// (an unknown outcome); a key not stored, the draft's error (800 when
    /// it had none: the keys did not reach a save, the file read-only); keys
    /// stored, the program's answer, unless its own catalog writes failed,
    /// which keeps that failure. So for a program whose only write is its
    /// keys this look decides both ways: a draft that failed and whose keys
    /// a later write stored answers as stored.
    pub fn error(&self, draft_failed: Option<u16>, own_writes: bool) -> Option<u16> {
        match self {
            IssuedKeysAre::Unknown(why) => Some(crate::hosting::save_error(
                &std::io::Error::other((*why).to_owned()),
            )),
            IssuedKeysAre::NotStored => Some(draft_failed.unwrap_or(800)),
            IssuedKeysAre::Stored => draft_failed.filter(|_| own_writes),
        }
    }
}

/// A catalog path with the last member id and modification count
/// GenerateMemberKey issued for it (schema-path.md 3.4).
pub type MemberKey = (Vec<Vec<u8>>, (u64, u64));

/// What a commit changed ([`Store::begin_commit`]), to put back when its
/// save fails, and the bookkeeping it does once at its end: each table's
/// record list and each changed index's counts are written once per
/// commit, not once per record, so a commit costs what its records do.
#[derive(Debug)]
pub struct Journal {
    /// Whether the tree had unsaved changes before the commit.
    was_dirty: bool,
    /// Whether the tree's root was a list (`OwnedNode::push`) before the
    /// commit: a file's root reads as not one, and a put at the top level
    /// makes it one.
    root_push: bool,
    /// In the order made.
    undo: Vec<Undo>,
    /// The paths of the nodes saved in `undo`: the first saving of a path
    /// holds its state before the commit, later ones are not needed.
    saved: BTreeSet<Vec<Vec<u8>>>,
    /// Per table, the record ids the commit added to and removed from its
    /// record list `[T, 0d]`.
    ids: BTreeMap<Vec<u8>, (Vec<u64>, Vec<u64>)>,
    /// Container names of deleted script records, by table. Their releases
    /// are checked once all the commit's record changes are in the tree.
    deleted_container_refs: BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, i32>>,
    /// The indexes whose counts the commit's records changed.
    recount: BTreeSet<Recount>,
    /// Per index that disagrees with the current rules, the records the
    /// commit changed or deleted in it and the keys each keeps: when the
    /// commit finishes they leave every other entry ([`Store::sweep_index`]).
    sweep: BTreeMap<IndexId, BTreeMap<u64, BTreeSet<Vec<u8>>>>,
    /// The derived state (`derived::Derived`, the decoded catalog and the
    /// accounts with it) as the store held it before the commit's first
    /// change to the catalogs it reads or to its accounts
    /// ([`Journal::schema_from`], [`Store::follow_accounts`]): a roll-back
    /// puts it back as it was. Reading the tree put back instead could fail
    /// (a catalog that does not decode, which the store served with the
    /// schema it last read) and keep the schema the commit decoded.
    schema: Option<Arc<Derived>>,
    /// Why a write of the commit was refused (for example, a posting
    /// whose RID encoding the writer cannot preserve). The refused write
    /// changed nothing. [`Store::save_commit`] takes back the rest of the
    /// commit and fails it.
    refused: Option<String>,
    /// The basis of stored calculations of each table op73 recomputed, as
    /// it was before ([`Store::update_table`]): a roll-back puts it back.
    basis: Vec<(Vec<u8>, Option<TableBasis>)>,
    /// Log lines that say what the change did, written once it is stored
    /// ([`Journal::take_lines`]) and dropped when it is taken back.
    lines: Vec<String>,
}

/// Where a save failed, for a test that makes saves fail on purpose
/// ([`crate::ServerHandle::failing_saves`]). Only in debug builds.
#[cfg(debug_assertions)]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveFault {
    /// Before the new copy is renamed over the file: the file on disk is
    /// as it was ([`SaveError::NotWritten`]).
    BeforeRename,
    /// Flushing the folder after the rename: the file holds the change,
    /// which may or may not survive a crash ([`SaveError::Unknown`]).
    FolderFlush,
}

/// Why a save did not confirm that the file on disk holds the tree
/// (docs/server-storage.md 2, rule 4; 5).
#[derive(Debug)]
pub enum SaveError {
    /// The file on disk is as it was before the save: it stopped before
    /// the new copy was renamed over the file (or the file is read-only,
    /// or takes no more changes), and the copy it made is gone.
    NotWritten(std::io::Error),
    /// The new copy was renamed over the file, but flushing the folder
    /// failed: the file holds the tree, which may or may not survive a
    /// crash. The store takes no more changes from now on
    /// ([`Store::writes_refused`]).
    Unknown(std::io::Error),
}

impl SaveError {
    /// The error the save failed with.
    pub fn error(&self) -> &std::io::Error {
        match self {
            SaveError::NotWritten(e) | SaveError::Unknown(e) => e,
        }
    }

    /// Whether the file on disk may hold the change.
    pub fn is_unknown(&self) -> bool {
        matches!(self, SaveError::Unknown(_))
    }
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::NotWritten(e) => e.fmt(f),
            SaveError::Unknown(e) => write!(
                f,
                "the file was written but could not be confirmed on disk ({e})"
            ),
        }
    }
}

impl From<std::io::Error> for SaveError {
    fn from(error: std::io::Error) -> Self {
        SaveError::NotWritten(error)
    }
}

/// An index whose counts leaf is due: table, field, index and counts keys.
type Recount = (Vec<u8>, Vec<u8>, u8, u8);

/// Pending field operations and the named cell keys used to resolve them.
type DeferredFieldWork<'a> = (&'a [(Vec<u8>, u32)], &'a BTreeSet<Vec<u8>>);

/// What a recalculation draws: each record's environment, and the unique
/// ids of the container data it stores.
type Draws<'a> = (
    &'a mut dyn FnMut() -> RecordEnvironment,
    &'a mut dyn FnMut() -> [u8; 16],
);

/// One index a record write changes: the field, the index (value 0, word
/// 1), and the record's entries in it before and after.
type Reindex = (Vec<u8>, u8, BTreeSet<Vec<u8>>, BTreeSet<Vec<u8>>);

impl Journal {
    /// Keep `derived`, the state the store derives from its catalogs,
    /// before the commit's first change to them ([`Journal::schema`]).
    fn schema_from(&mut self, derived: &Arc<Derived>) {
        if self.schema.is_none() {
            self.schema = Some(derived.clone());
        }
    }

    /// Keep the node at `path` of `root` as it is now, unless this commit
    /// already kept it, and what a change there would create on its way
    /// ([`Self::save_parents`]): a missing parent is kept as missing, so a
    /// roll-back removes all the change created, the parent included.
    fn save_node(&mut self, root: &OwnedNode, path: Vec<Vec<u8>>) {
        if self.save_parents(root, &path) {
            self.keep_node(root, path);
        }
    }

    /// Keep the node at `path` of `root` as it is now, unless this commit
    /// already kept it; its parents are not looked at.
    fn keep_node(&mut self, root: &OwnedNode, path: Vec<Vec<u8>>) {
        if self.saved.contains(&path) {
            return;
        }
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let old = root.at(&segments).cloned();
        self.saved.insert(path.clone());
        self.undo.push(Undo::Node(path, old));
    }

    /// Keep what putting a node at `path` of `root` changes besides the
    /// node there: the parents `OwnedNode::ensure` would create, or turn
    /// into lists. The first of them that is missing is kept as missing,
    /// so a roll-back removes all it created; a node on the way that is not
    /// a list yet (it has no children) is kept as it is. Returns whether
    /// every parent exists as a list already.
    fn save_parents(&mut self, root: &OwnedNode, path: &[Vec<u8>]) -> bool {
        let Some((_, parents)) = path.split_last() else {
            return true;
        };
        let mut node = root;
        for depth in 1..=parents.len() {
            match node.child(&parents[depth - 1]) {
                Some(child) if child.push => node = child,
                _ => {
                    self.keep_node(root, parents[..depth].to_vec());
                    return false;
                }
            }
        }
        true
    }

    /// Keep what putting a node at `path` of `root` changes: its parents
    /// ([`Self::save_parents`]) and the node there now, copied.
    fn save_path(&mut self, root: &OwnedNode, path: Vec<Vec<u8>>) {
        self.save_node(root, path);
    }

    /// Remove the node at `path` of `root`, kept here (moved, not copied)
    /// unless this commit already kept the path, and return whether there
    /// was one.
    fn take(&mut self, root: &mut OwnedNode, path: Vec<Vec<u8>>) -> bool {
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let old = root.remove(&segments);
        let was = old.is_some();
        if self.saved.insert(path.clone()) {
            self.undo.push(Undo::Node(path, old));
        }
        was
    }

    /// Put `node` at `path` of `root`, replacing what is there, which is
    /// kept here (moved) with the parents the put creates.
    fn replace(&mut self, root: &mut OwnedNode, path: Vec<Vec<u8>>, node: OwnedNode) {
        self.save_parents(root, &path);
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let old = root.remove(&segments);
        root.put(&segments, node);
        if self.saved.insert(path.clone()) {
            self.undo.push(Undo::Node(path, old));
        }
    }

    /// Keep record `record` of `table` as it is now, before a change of its
    /// own made in place (op73's field work), unless this commit already
    /// kept it.
    fn save_record(&mut self, table: &[u8], record: &OwnedNode) {
        let path = vec![table.to_vec(), vec![keys::RECORDS], record.segment.clone()];
        if self.saved.insert(path.clone()) {
            self.undo.push(Undo::Node(path, Some(record.clone())));
        }
    }

    /// Take `inner`'s changes into this journal, after its own: a part of
    /// the change that kept a journal of its own and was not taken back.
    fn absorb(&mut self, inner: Journal) {
        self.saved.extend(inner.saved);
        self.undo.extend(inner.undo);
        self.lines.extend(inner.lines);
    }

    /// Note a log line that says what the change did, written once it is
    /// stored.
    fn note(&mut self, line: String) {
        self.lines.push(line);
    }

    /// The log lines that say what the change did ([`Self::note`]), for
    /// the caller to write once it is stored.
    pub fn take_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.lines)
    }

    /// Whether the change has effects to store or take back: it changed
    /// the tree, op73's basis, or a write of it was refused.
    pub fn changed(&self) -> bool {
        !self.undo.is_empty()
            || !self.basis.is_empty()
            || self.refused.is_some()
            || !self.ids.is_empty()
    }
}

/// Write `line` about the file named `name` into `journal`'s log lines, or
/// now without one.
fn note(name: &str, journal: Option<&mut Journal>, line: String) {
    let line = format!("{name}: {line}");
    match journal {
        Some(journal) => journal.note(line),
        None => info!("{line}"),
    }
}

#[derive(Debug)]
enum Undo {
    /// A record of a table as it was (`None`: it did not exist).
    Record(Vec<u8>, u64, Option<OwnedNode>),
    /// The node at a path as it was.
    Node(Vec<Vec<u8>>, Option<OwnedNode>),
}

/// Where a save writes the file's new copy: `<file>.omsave` beside it.
pub(crate) fn save_copy_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".omsave");
    PathBuf::from(temporary)
}

/// The name of a save's new copy of the file named `name`, beside it.
fn save_copy_name(name: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut temporary = name.to_owned();
    temporary.push(".omsave");
    temporary
}

/// Whether anything is under the name of a save's new copy beside the file
/// named `name` in `folder` (`<name>.omsave`, [`save_copy_path`]), looked
/// at without following a link: a copy a save left when it stopped part
/// way, or anything else. Saves of the file fail while it is there, and
/// nothing removes it: a server opening the file raises an alert that
/// names it (docs/server-storage.md 5).
pub(crate) fn copy_beside(folder: &Folder, name: &std::ffi::OsStr) -> std::io::Result<bool> {
    Ok(folder.entry(&save_copy_name(name))?.is_some())
}

/// Whether `file`, the copy a save created, is still entry `name` of the
/// file's folder.
fn owned_copy(folder: &Folder, file: &std::fs::File, name: &std::ffi::OsStr) -> bool {
    #[cfg(unix)]
    {
        folder.is_entry(file, name).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        // No inode identity (Windows): a regular file where the save made it.
        let _ = file;
        folder.is_regular(name)
    }
}

/// The paths and reasons of nodes the file cannot hold, for a log line.
pub(crate) fn describe(unwritable: &[Unwritable]) -> String {
    unwritable
        .iter()
        .map(|u| {
            let path: Vec<String> = u
                .path
                .iter()
                .map(|s| {
                    let hex: String = s.iter().take(8).map(|b| format!("{b:02x}")).collect();
                    if s.len() > 8 {
                        format!("{hex}..({} bytes)", s.len())
                    } else {
                        hex
                    }
                })
                .collect();
            format!("[{}]: {}", path.join(", "), u.reason)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A node that holds a value a write would have gone beneath, for a log line.
fn describe_value(path: &[Vec<u8>]) -> String {
    describe(&[Unwritable {
        path: path.to_vec(),
        reason: "a value cannot become a list",
    }])
}

impl Store {
    /// The file is served privately to a desktop client
    /// ([`crate::LocalHost`]): its calculations compile as FileMaker Pro's.
    pub(crate) fn serve_desktop(&mut self) {
        Arc::make_mut(&mut self.derived).serve_desktop();
    }

    /// Load every key of `file` into memory.
    pub fn load(file: &HbamFile, name: &str) -> om_hbam::Result<Self> {
        let root = file.key_tree()?.root.to_owned_node();
        let catalog = file.catalog()?;
        let derived = Derived::new(&TreeView::new(&root, 0), catalog, name)
            .expect("the file engine's tree never fails a read");
        let mut store = Self {
            root,
            version: 0,
            derived: Arc::new(derived),
            name: name.to_string(),
            basis: HashMap::new(),
            superblock: file.superblock().raw.to_vec(),
            dirty: false,
            saves: 0,
            #[cfg(debug_assertions)]
            fail_commit_saves: false,
            #[cfg(debug_assertions)]
            save_fault: None,
            unknown: None,
            generation: 0,
            accounts_at: 0,
            waiting: Waiting::default(),
            verified: (0, BTreeMap::new()),
            lock: None,
            file: None,
            read_only: None,
        };
        // A file holding keys or values the writer cannot write (a reader
        // accepts longer key segments and one more level) is served but
        // never written: saving it would fail, and dropping what it holds
        // would lose data.
        let unwritable = unwritable_in_tree(&store.root, 3);
        if !unwritable.is_empty() {
            error!(
                "{name}: the file holds content this server cannot write back ({}); \
                 it is hosted read-only",
                describe(&unwritable)
            );
            store.read_only = Some(ReadOnlyCause::Unwritable);
        }
        // Tables with deferred work pending (the server stopped between a
        // client's op13 and its op73) get no basis: [`Store::recover`]
        // recomputes all their stored calculations.
        for table in &store.derived.catalog().tables {
            let t = uint_segment(table.id);
            if store.node(&Self::deferred_path(&t)).is_none() {
                store
                    .basis
                    .insert(t, recalc::table_basis(&store.root, table));
            }
        }
        Ok(store)
    }

    /// `[T, 03, 01, fd]`: a table's deferred field work (schema-path.md 7.2).
    fn deferred_path(table: &[u8]) -> [&[u8]; 4] {
        [
            table,
            &[keys::FIELDS],
            &[keys::FIELD_CATALOG_INDEX],
            &[keys::DEFERRED],
        ]
    }

    /// Finish deferred field work the file was saved with: a server that
    /// stopped during or before a client's op73 left the work list in the
    /// file (the save that would have removed it never happened), so it
    /// runs now, before the file is served, as op73 would have. Returns
    /// whether anything changed.
    pub fn recover(
        &mut self,
        mut journal: Option<&mut Journal>,
        environment: &mut dyn FnMut() -> RecordEnvironment,
        unique_id: &mut dyn FnMut() -> [u8; 16],
    ) -> bool {
        let pending: Vec<Vec<u8>> = self
            .derived
            .catalog()
            .tables
            .iter()
            .map(|t| uint_segment(t.id))
            .filter(|t| self.node(&Self::deferred_path(t)).is_some())
            .collect();
        if self.is_read_only() {
            return false;
        }
        // A failed recovery must leave the file and the pending work intact.
        for table in &pending {
            if let Err(reason) = self.deferred_work(table) {
                warn!(
                    "{}: table {table:02x?}: deferred work kept, hosting read-only: {reason}",
                    self.name
                );
                self.set_read_only(ReadOnlyCause::DeferredWork);
                return false;
            }
        }
        let mut changed = false;
        for table in pending {
            note(
                &self.name,
                journal.as_deref_mut(),
                format!("finished the deferred field work of table {table:02x?}"),
            );
            // The preflight checked every pending table before any changed.
            changed |= self
                .update_table(journal.as_deref_mut(), &table, environment, unique_id)
                .unwrap_or(false);
        }
        changed
    }

    fn deferred_work(&self, table: &[u8]) -> Result<Vec<(Vec<u8>, u32)>, &'static str> {
        let Some(list) = self.node(&Self::deferred_path(table)) else {
            return Ok(Vec::new());
        };
        if holds_value(list) {
            return Err("the deferred work list is not a list");
        }
        list.children
            .iter()
            .map(|entry| {
                let field = be_uint(&entry.segment)
                    .filter(|id| *id != 0 && uint_segment(*id) == entry.segment)
                    .ok_or("a deferred field key does not decode")?;
                let bits = entry
                    .value
                    .as_deref()
                    .filter(|_| entry.children.is_empty())
                    .and_then(|bytes| bytes.try_into().ok())
                    .map(u32::from_be_bytes)
                    .filter(|bits| bits & !deferred::KNOWN == 0)
                    .ok_or("the deferred work uses unknown metadata")?;
                if bits & (deferred::REMOVES_VALUES | deferred::TO_DATE | deferred::TO_TIMESTAMP)
                    != 0
                {
                    let old = self.basis.get(table).and_then(|basis| basis.get(&field));
                    let (container, global) =
                        old.and_then(recalc::FieldBasis::known_storage).ok_or(
                            "deferred value removal or conversion has no known old field type",
                        )?;
                    if bits & deferred::REMOVES_VALUES != 0
                        && (container != (bits & deferred::CONTAINER != 0)
                            || global != (bits & deferred::GLOBAL != 0))
                    {
                        return Err(
                            "deferred value removal disagrees with the old field type or storage",
                        );
                    }
                    if bits & deferred::FEWER != 0
                        && !self
                            .defined_repetitions(table, &entry.segment)
                            .is_some_and(|n| n >= 1)
                    {
                        return Err(
                            "deferred repetition removal has no known new repetition count",
                        );
                    }
                }
                Ok((entry.segment.clone(), bits))
            })
            .collect()
    }

    /// The file's own language (`Locale::Type`), which an index in
    /// language 0 keys text in (query-program.md 7.2): its locale record's,
    /// en-US's English when it has none, and `None` when the record does
    /// not decode, rather than English, which would key a German file's `ä`
    /// as `a`.
    pub fn file_language(&self) -> Option<u32> {
        if self.derived.decoded_locale().is_none() && self.node(&LOCALE).is_some() {
            return None;
        }
        Some(u32::from(self.locale().language.id()))
    }

    /// The file's locale and formats: its record when it decodes, else
    /// en-US with the date order its stored dates use.
    pub fn locale(&self) -> Locale {
        self.derived.locale()
    }

    /// Hold `lock` on the file for as long as the store has it, moving it
    /// to each file a save writes ([`crate::filelock`]).
    pub(crate) fn set_lock(&mut self, lock: FileLock) {
        self.lock = Some(lock);
    }

    /// Refuse to write the file: a save fails without touching it, and its
    /// changes stay in memory (a commit's are taken back,
    /// [`Store::save_commit`]).
    pub fn set_read_only(&mut self, cause: ReadOnlyCause) {
        self.read_only.get_or_insert(cause);
    }

    /// Why the file may not be written ([`Store::set_read_only`]), the
    /// first cause that made it so.
    pub fn read_only_cause(&self) -> Option<ReadOnlyCause> {
        self.read_only
    }

    /// Whether the file may not be written: [`Store::set_read_only`], or it
    /// loaded with content the writer cannot write back.
    pub fn is_read_only(&self) -> bool {
        self.read_only.is_some()
    }

    /// How many saves have written the file since it was loaded. The file
    /// on disk changes only by a save, which replaces it whole, so an
    /// unchanged count read under the store's lock means the file at its
    /// path is the one an earlier read saw (a backup links its last copy
    /// instead of copying the file again, `crate::backups`).
    pub fn saves(&self) -> u64 {
        self.saves
    }

    /// Whether the tree has changes the file on disk lacks: a save failed
    /// and they wait in memory for the next one.
    pub fn has_unsaved_changes(&self) -> bool {
        self.dirty
    }

    /// Write the tree to the file `at` if it has unsaved changes: a complete new
    /// file (`om_hbam::writer::rebuild`) written beside it, flushed, given
    /// the old file's permissions, then renamed over it, so a crash leaves
    /// the old file or the new one (hbam-writer.md 6.2). Returns whether it
    /// wrote.
    ///
    /// The new copy, [`save_copy_path`], is created only where nothing is:
    /// an entry of that name the store did not make (a copy a crash left,
    /// a link to another file) stops the save, untouched ([`copy_beside`]).
    /// A store holding
    /// the file's lock first checks that its locked descriptor is still the
    /// file's entry in its folder, with no other names (a file another
    /// program replaced or linked is not saved over), and locks the new
    /// copy before the rename. On an error before the rename the file is
    /// the old one, the copy the store made is removed (only it), the tree
    /// stays unsaved and the error is [`SaveError::NotWritten`]. Once the
    /// rename succeeded the file is the new one: a failure to flush the
    /// folder after it is [`SaveError::Unknown`], since the rename may not
    /// survive a crash, and the store takes no more changes until the file
    /// is hosted again ([`Store::writes_refused`]). Every step goes through
    /// the file's folder, opened again and checked to be the one the file
    /// was hosted from ([`FileAt::folder`]): a folder of the path swapped
    /// for a link, or moved, since then fails the save rather than move it
    /// (#468).
    ///
    /// The copy is the whole database, password verifiers included. Until
    /// it has the old file's permissions it is owner-only (0600 on Unix,
    /// given as it is created, whatever the umask), so no one the file
    /// keeps out can open it while it is written: a descriptor opened then
    /// would go on reading it whatever its permissions became. With no file
    /// there it is a new file from the start (0666 less the umask);
    /// when the old file's permissions cannot be read, nothing is saved.
    pub fn save(&mut self, at: &FileAt) -> Result<bool, SaveError> {
        self.save_observed(at, &mut |_| {})
    }

    /// [`Store::save`], calling `created` with the new copy's path once it
    /// exists, before anything is written to it (tests).
    fn save_observed(
        &mut self,
        at: &FileAt,
        created: &mut dyn FnMut(&Path),
    ) -> Result<bool, SaveError> {
        use std::io::Error;
        if !self.dirty {
            return Ok(false);
        }
        if let Some(why) = &self.unknown {
            return Err(SaveError::NotWritten(Error::other(why.clone())));
        }
        let folder = self.write_new_copy(at, created)?;
        self.dirty = false;
        self.saves += 1;
        #[cfg(debug_assertions)]
        let flushed = match self.save_fault {
            Some(SaveFault::FolderFlush) => Err(Error::other("a test makes the folder flush fail")),
            _ => folder.sync(),
        };
        #[cfg(not(debug_assertions))]
        let flushed = folder.sync();
        if let Err(e) = flushed {
            let why = format!(
                "the file takes no more changes until it is hosted again: its last save \
                 could not flush the folder of {} ({e}), so whether the file on disk keeps \
                 that change is unknown",
                at.path().display()
            );
            error!("{}: {why}", self.name);
            self.unknown = Some(why);
            return Err(SaveError::Unknown(e));
        }
        Ok(true)
    }

    /// Why the file takes no more changes, when a save's outcome is
    /// unknown ([`SaveError::Unknown`]): every later save is refused, and
    /// the engine refuses every operation that would change the file.
    pub fn writes_refused(&self) -> Option<&str> {
        self.unknown.as_deref()
    }

    /// The steps of [`Store::save`] up to and including the rename: an
    /// error is a save that did not write the file, which is as it was.
    /// After the rename the store holds the new file's identity, lock and
    /// page 0. Returns the folder the rename was made in, to flush.
    fn write_new_copy(
        &mut self,
        at: &FileAt,
        created: &mut dyn FnMut(&Path),
    ) -> std::io::Result<Folder> {
        use std::io::{Error, ErrorKind};
        if self.read_only.is_some() {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "the file is open read-only",
            ));
        }
        // Every step goes through the file's folder, checked to be the one
        // the file was hosted from (#468), by the file's name.
        let name = at.name();
        let folder = at.folder_now()?;
        let permissions = match &self.lock {
            Some(lock) => {
                let current = folder.is_entry(lock.file(), name)?
                    && crate::filelock::link_count(lock.file())? == 1;
                if !current {
                    return Err(Error::other(
                        "the file was replaced or given another name by another program; \
                         it was not saved over",
                    ));
                }
                Some(lock.file().metadata()?.permissions())
            }
            // One look at the entry, never through a link: whether it is
            // the file this store was read from or last saved as, and its
            // permissions. A folder of the path swapped for a link to other
            // content, to an empty folder, or to a folder whose entry links
            // back to the file, is not saved into (#468).
            None => match (self.file, folder.entry(name)?) {
                (Some(file), Some(entry)) if entry.regular && entry.id == Some(file) => {
                    Some(entry.permissions)
                }
                (Some(_), _) => {
                    return Err(Error::other(format!(
                        "{} no longer leads to the file this server read or last saved \
                         there (a folder of the path, or the file, was replaced); it was \
                         not saved",
                        at.path().display()
                    )))
                }
                (None, Some(entry)) if entry.regular => Some(entry.permissions),
                (None, Some(_)) => {
                    return Err(Error::other(format!(
                        "{} is not a regular file; it was not saved over",
                        at.path().display()
                    )))
                }
                (None, None) => None,
            },
        };
        let bytes = om_hbam::writer::rebuild(&self.root, &self.superblock)
            .map_err(|e| Error::new(ErrorKind::InvalidData, e.to_string()))?;
        let temporary = save_copy_name(name);
        let shown = save_copy_path(at.place());
        // Owner-only until it takes the old file's permissions; a new
        // file's (0666 less the umask) when there is none to take.
        let file = folder
            .create_new(&temporary, permissions.is_some())
            .map_err(|e| {
                if e.kind() == ErrorKind::AlreadyExists {
                    Error::new(
                        e.kind(),
                        format!(
                            "{} is in the way of the save's new copy (left by an earlier save?)",
                            shown.display()
                        ),
                    )
                } else {
                    e
                }
            })?;
        created(&shown);
        // The copy is this save's from here on: on an error it removes the
        // entry only while it is still that copy.
        let owner = match file.try_clone() {
            Ok(owner) => owner,
            Err(e) => {
                if owned_copy(&folder, &file, &temporary) {
                    let _ = folder.remove(&temporary);
                }
                return Err(e);
            }
        };
        let saved = {
            let written = (|| {
                let mut file = file;
                file.write_all(&bytes)?;
                file.sync_all()?;
                if let Some(permissions) = permissions {
                    file.set_permissions(permissions)?;
                }
                match self.lock.as_ref().map(FileLock::kind) {
                    Some(kind) => FileLock::acquire(file, kind).map(Some).map_err(|(e, _)| e),
                    None => Ok(None),
                }
            })();
            let renamed = written.and_then(|lock| {
                // Read the staged file's identity before publication. The
                // destination name could refer to another file after rename.
                let identity = if self.file.is_some() {
                    let staged = crate::anchored::file_identity(&owner)?;
                    Some(staged.ok_or_else(|| Error::other("the new copy has no file identity"))?)
                } else {
                    None
                };
                if !owned_copy(&folder, &owner, &temporary) {
                    return Err(Error::other("the save's new copy was replaced"));
                }
                #[cfg(debug_assertions)]
                if self.save_fault == Some(SaveFault::BeforeRename) {
                    return Err(Error::other("a test makes saves fail before the rename"));
                }
                folder.rename(&temporary, name)?;
                Ok((lock, identity))
            });
            if renamed.is_err() && owned_copy(&folder, &owner, &temporary) {
                let _ = folder.remove(&temporary);
            }
            renamed
        };
        let (lock, identity) = saved?;
        self.file = identity;
        if lock.is_some() {
            // The old file is gone from the path: its lock guards nothing.
            self.lock = lock;
        }
        self.superblock = bytes[..self.superblock.len()].to_vec();
        Ok(folder)
    }

    /// The file as the store last loaded or saved it, for a copy of it: a
    /// new descriptor of the one the store holds locked, which names that
    /// version whatever later saves rename over its path, to be read by
    /// position so as not to move the offset the two share. It is a
    /// [`SavedFile`], which only reads: the descriptor the store holds is
    /// open for writing, and only the engine's save may write the file.
    /// `None` while the tree has changes the file lacks (a failed save's)
    /// or when the store holds no lock, where the file at the path could be
    /// another program's: such a copy is built from the tree
    /// ([`Store::compacted`]).
    #[cfg(target_os = "linux")]
    pub fn saved_file(&self) -> Option<std::io::Result<SavedFile>> {
        if self.dirty {
            return None;
        }
        self.lock
            .as_ref()
            .map(|lock| lock.file().try_clone().map(SavedFile))
    }

    /// The tree as a complete new file, packed (`om_hbam::writer::rebuild`,
    /// hbam-writer.md 7.2): the shape of FileMaker's compacted copies
    /// (hbam-writer.md 9.3). A tree holding content the writer cannot write
    /// is refused, as its saves are.
    #[cfg(target_os = "linux")]
    pub fn compacted(&self) -> std::io::Result<Vec<u8>> {
        om_hbam::writer::rebuild(&self.root, &self.superblock)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }

    /// Start a commit whose save must succeed for it to count
    /// ([`Store::save_commit`]).
    pub fn begin_commit(&self) -> Journal {
        Journal {
            was_dirty: self.dirty,
            root_push: self.root.push,
            undo: Vec::new(),
            saved: BTreeSet::new(),
            ids: BTreeMap::new(),
            deleted_container_refs: BTreeMap::new(),
            recount: BTreeSet::new(),
            sweep: BTreeMap::new(),
            schema: None,
            refused: None,
            basis: Vec::new(),
            lines: Vec::new(),
        }
    }

    /// [`Store::write_record`] within a commit: the record's old version
    /// goes into `journal`, and the record list and index counts are
    /// written when the commit is saved ([`Store::save_commit`]).
    pub fn write_record_in(
        &mut self,
        journal: &mut Journal,
        table: &[u8],
        id: u64,
        record: Option<OwnedNode>,
    ) {
        let old = self
            .node(&[table, &[keys::RECORDS], &serial::encode(id)])
            .cloned();
        journal.save_node(&self.root, vec![table.to_vec(), vec![keys::RECORD_IDS]]);
        // The first record of a table creates its record list `[T, 05]`.
        let at = vec![table.to_vec(), vec![keys::RECORDS], serial::encode(id)];
        journal.save_parents(&self.root, &at);
        let (existed, exists) = (old.is_some(), record.is_some());
        let mut recount = std::mem::take(&mut journal.recount);
        let put = self.put_record(
            table,
            id,
            old.as_ref(),
            record,
            Some(journal),
            &mut recount,
            true,
        );
        journal.recount = recount;
        if let Err(reason) = put {
            journal.refused.get_or_insert(reason);
            return;
        }
        journal.undo.push(Undo::Record(table.to_vec(), id, old));
        let ids = journal.ids.entry(table.to_vec()).or_default();
        match (existed, exists) {
            (false, true) => ids.0.push(id),
            (true, false) => ids.1.push(id),
            _ => {}
        }
    }

    /// The bookkeeping a commit does once: each table's record list with
    /// the ids it added and removed, each changed index's counts. The
    /// nodes it replaces go into `journal`.
    fn finish_commit(&mut self, journal: &mut Journal) {
        for (table, (added, removed)) in std::mem::take(&mut journal.ids) {
            self.update_record_ids(&table, |ids| {
                for id in added {
                    ids.insert(id);
                }
                if !removed.is_empty() {
                    *ids = ids.and_not(&removed.into_iter().collect());
                }
            });
        }
        for ((table, field, kind), records) in std::mem::take(&mut journal.sweep) {
            let mut recount = std::mem::take(&mut journal.recount);
            self.sweep_index(
                Some(journal),
                &mut recount,
                (&table, &field, kind),
                &records,
            );
            journal.recount = recount;
        }
        for (table, field, index, counts) in std::mem::take(&mut journal.recount) {
            journal.save_node(
                &self.root,
                vec![
                    table.clone(),
                    vec![keys::INDEXES],
                    field.clone(),
                    vec![counts],
                ],
            );
            self.recount_index(&table, &field, index, counts);
        }
    }

    /// Within a commit of a server script's record of `table`, count its
    /// container references as FileMaker Pro's op87 counts a client's
    /// (containers.md 5.1): a value the record holds now and did not
    /// before (one Duplicate Record copied) takes a reference (+1), one it
    /// held and no longer does (the record is deleted, 7.3) releases it
    /// (-1, deleting the member at 0). `old` is the record as stored,
    /// `new` as it will be (`None`: absent). Without the +1 two records
    /// share a member counted once and the first released deletes the
    /// other's data; without the -1 a deleted record's data stays behind.
    /// Everything changed goes into `journal`, so a failed save puts it
    /// back, a deleted member included. `counted` names the references
    /// of `new` that [`Store::store_container_in`] already counted.
    pub fn count_containers_in(
        &mut self,
        journal: &mut Journal,
        table: &[u8],
        old: Option<&OwnedNode>,
        new: Option<&OwnedNode>,
        counted: &[Vec<u8>],
    ) {
        if new.is_none() {
            for cell in old.iter().flat_map(|r| &r.children) {
                if self.container_cell(table, &cell.segment) {
                    if let Some(member) = member_key(cell) {
                        let count = journal
                            .deleted_container_refs
                            .entry(table.to_vec())
                            .or_default()
                            .entry(member)
                            .or_default();
                        *count = count.saturating_add(1);
                    }
                }
            }
            return;
        }
        let mut deltas: BTreeMap<Vec<u8>, i32> = BTreeMap::new();
        for member in counted {
            *deltas.entry(member.clone()).or_default() -= 1;
        }
        for (record, delta) in [(old, -1), (new, 1)] {
            for cell in record.iter().flat_map(|r| &r.children) {
                if self.container_cell(table, &cell.segment) {
                    if let Some(member) = member_key(cell) {
                        *deltas.entry(member).or_default() += delta;
                    }
                }
            }
        }
        self.adjust_container_counts(journal, table, deltas);
    }

    /// Retained text from a converted field can name a member without a
    /// counted reference. Protect the names that remain in records and
    /// globals, scanning each affected table only once per script commit.
    pub fn release_deleted_record_containers(&mut self, journal: &mut Journal) {
        for (table, removed) in std::mem::take(&mut journal.deleted_container_refs) {
            let naming = self.naming_cells(&table, &[], &BTreeSet::new());
            let deltas = removed
                .into_iter()
                .map(|(member, removed)| {
                    let surviving = naming.get(&member).copied().unwrap_or_default();
                    let release =
                        self.guarded_container_release(&table, &member, 0, surviving, removed);
                    (member, -release)
                })
                .collect();
            self.adjust_container_counts(journal, &table, deltas);
        }
    }

    fn adjust_container_counts(
        &mut self,
        journal: &mut Journal,
        table: &[u8],
        deltas: BTreeMap<Vec<u8>, i32>,
    ) {
        let library: [&[u8]; 2] = [table, &[LIBRARY]];
        for (member, delta) in deltas.into_iter().filter(|(_, d)| *d != 0) {
            if let Some(reason) = self.library_refusal(&library, None, Some((&member, delta))) {
                journal.refused.get_or_insert(reason);
                continue;
            }
            // The member's counts, or the member with its entries in the
            // header when it goes: the decision the adjustment makes.
            let Some(changed) = self
                .node(&library)
                .and_then(|l| library::adjust_keys(l, &member, delta))
            else {
                continue;
            };
            let base = vec![table.to_vec(), vec![LIBRARY]];
            for keys in changed {
                let mut path = base.clone();
                path.extend(keys);
                journal.save_node(&self.root, path);
            }
            if let Some(library) = self.root.at_mut(&library) {
                library::adjust(library, &member, delta);
                self.dirty = true;
            }
        }
    }

    /// References that remain after deferred value removal, including
    /// globals and cells kept when an old field's record cannot advance.
    fn naming_cells(
        &self,
        table: &[u8],
        work: &[(Vec<u8>, u32)],
        named: &BTreeSet<Vec<u8>>,
    ) -> BTreeMap<Vec<u8>, i64> {
        let mut naming = BTreeMap::new();
        for storage in [keys::RECORDS, keys::GLOBALS] {
            let old: BTreeSet<_> = work
                .iter()
                .filter(|(_, bits)| {
                    bits & deferred::CONTAINER != 0
                        && (bits & deferred::GLOBAL != 0) == (storage == keys::GLOBALS)
                })
                .flat_map(|(field, _)| self.cells_from(table, field, 1, named))
                .collect();
            let Some(values) = self.node(&[table, &[storage]]) else {
                continue;
            };
            let mut count = |cell: &OwnedNode| {
                if old.contains(&cell.segment)
                    || (self.container_cell(table, &cell.segment)
                        && (!named.contains(&cell.segment)
                            || self.field_type(table, &cell.segment) == Some(DataType::Container)))
                {
                    if let Some(member) = member_key(cell) {
                        *naming.entry(member).or_default() += 1;
                    }
                }
            };
            if storage == keys::GLOBALS {
                values.children.iter().for_each(&mut count);
            } else {
                for record in &values.children {
                    record.children.iter().for_each(&mut count);
                }
            }
        }
        naming
    }

    /// Releases can spend only pre-existing references left after
    /// protecting surviving names. Acquisitions from this change are
    /// reserved for its new cells, even when old text named the same key.
    fn guarded_container_release(
        &self,
        table: &[u8],
        member: &[u8],
        counted: i32,
        surviving: i64,
        removed: i32,
    ) -> i32 {
        let stored = self
            .library_member(&[table, &[LIBRARY]], member)
            .map_or(0, library::reference_count);
        let available = i64::from(stored) - i64::from(counted) - surviving.max(0);
        let release = available.clamp(0, i64::from(removed)) as i32;
        if release < removed {
            warn!(
                "{}: table {table:02x?} member {member:02x?}: held {} container release(s)",
                self.name,
                removed - release
            );
        }
        release
    }

    /// Store container data a server script set into a field of a record
    /// of `table` in the table's library, within a commit (containers.md
    /// 3.4, as op88 stores a client's upload): a member that already holds
    /// the data counts one more reference, otherwise a new member holds it
    /// with count 1, the library made on first use. Returns the member; the
    /// reference is counted, for [`Store::count_containers_in`]. What the
    /// store changes goes into `journal`. A new member's unique id comes
    /// from `unique_id`.
    pub fn store_container_in(
        &mut self,
        journal: &mut Journal,
        table: &[u8],
        data: BinaryData,
        unique_id: &mut dyn FnMut() -> [u8; 16],
    ) -> Vec<u8> {
        self.store_container_with(journal, table, data, unique_id)
    }

    /// [`Store::store_container_in`], a new member's unique id taken from
    /// `unique_id`.
    fn store_container_with(
        &mut self,
        journal: &mut Journal,
        table: &[u8],
        data: BinaryData,
        unique_id: &mut dyn FnMut() -> [u8; 16],
    ) -> Vec<u8> {
        let base = vec![table.to_vec(), vec![LIBRARY]];
        if let Some(reason) = self.library_refusal(&[table, &[LIBRARY]], Some(&data), None) {
            journal.refused.get_or_insert(reason);
            return Vec::new();
        }
        // One decision, which the journal keeps and the store carries out.
        let library = self.node(&[table, &[LIBRARY]]);
        let plan = library::store_plan(library, &data.md5);
        match plan.keys(library) {
            None => journal.save_node(&self.root, base),
            Some(keys) => {
                for keys in keys {
                    let mut path = base.clone();
                    path.extend(keys);
                    journal.save_node(&self.root, path);
                }
            }
        }
        self.dirty = true;
        let library = self.root.ensure(&[table, &[LIBRARY]]);
        match library::store_as(library, data, &plan, unique_id) {
            Some(Stored::New(member) | Stored::Existing(member)) => member,
            // Not reached: `library_refusal` checked the same counters.
            None => {
                journal
                    .refused
                    .get_or_insert_with(|| "a library counter cannot advance".to_string());
                Vec::new()
            }
        }
    }

    /// After Delete All Records or Truncate Table, within their commit: a
    /// table left without records loses its whole library `[T, 1f]`, header
    /// and orphans included, as on FileMaker Server's engine (script oracle
    /// s34-containers-release; record by record its header stays, 5.3).
    /// Returns whether it did; the library goes into `journal`.
    pub fn drop_emptied_library_in(&mut self, journal: &mut Journal, table: &[u8]) -> bool {
        let empty = self
            .node(&[table, &[keys::RECORDS]])
            .map_or(true, |records| records.children.is_empty());
        if !empty || self.node(&[table, &[LIBRARY]]).is_none() {
            return false;
        }
        journal.save_node(&self.root, vec![table.to_vec(), vec![LIBRARY]]);
        self.root.remove(&[table, &[LIBRARY]]);
        self.dirty = true;
        true
    }

    /// Whether `cell` of a record of `table` is a container field's value
    /// (repetition n of field F is the cell `F * 256 + n`).
    pub fn container_cell(&self, table: &[u8], cell: &[u8]) -> bool {
        let Some(id) = be_uint(cell) else {
            return false;
        };
        let field = match self.field_type(table, cell) {
            Some(_) => id,
            None if id & 0xff >= 2 => id >> 8,
            None => return false,
        };
        self.field_type(table, &uint_segment(field)) == Some(DataType::Container)
    }

    /// [`Store::serial_value`] replacing a field's next serial value within
    /// a commit: the old node goes into `journal`.
    pub fn set_serial_in(
        &mut self,
        journal: &mut Journal,
        table: &[u8],
        field: &[u8],
        value: &str,
    ) -> Option<String> {
        let path: Vec<Vec<u8>> = vec![
            table.to_vec(),
            vec![keys::FIELDS],
            vec![keys::FIELD_DEFINITIONS],
            field.to_vec(),
            vec![keys::SERIAL_VALUE],
        ];
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let old = self.node(&segments).cloned();
        let current = self.serial_value(table, field, Some(value), None)?;
        journal.undo.push(Undo::Node(path, old));
        Some(current)
    }

    /// Write a commit to `path` ([`Store::save`]). When the file cannot be
    /// written, everything the commit changed is put back, newest first,
    /// and the error returned: the tree never holds a commit the file
    /// lacks (write-path.md 5; OpenMaker's rule, FileMaker Server's own
    /// handling of a failed write is not observed). A commit with a refused
    /// write ([`Journal`]'s `refused`: it would have changed a value it does
    /// not name, or left something the file cannot hold) is put back the
    /// same way without a save, and fails as content the file cannot hold
    /// (`InvalidData`). When the save failed on content that was in the
    /// tree before the commit, the field indexes among it are taken out
    /// ([`Store::drop_unwritable_indexes`]), so the next commit is saved;
    /// anything else stays as it is and is logged.
    pub fn save_commit(&mut self, at: &FileAt, journal: Journal) -> Result<(), SaveError> {
        self.save_journal(at, journal, true)
    }

    /// [`Store::save_commit`] for a change that is not a commit (op71's
    /// reservation, op72's serial, the schema and container operations,
    /// op240's Save): the same, except that the test switch that makes
    /// commits fail ([`crate::ServerHandle::failing_commit_saves`]) does not
    /// apply.
    pub fn save_change(&mut self, at: &FileAt, journal: Journal) -> Result<(), SaveError> {
        self.save_journal(at, journal, false)
    }

    fn save_journal(
        &mut self,
        at: &FileAt,
        mut journal: Journal,
        commit: bool,
    ) -> Result<(), SaveError> {
        #[cfg(not(debug_assertions))]
        let _ = commit;
        let saved = self.guarded(&mut journal, |store, journal| {
            if let Some(reason) = journal.refused.take() {
                return Err(SaveError::NotWritten(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("a write of the commit was refused: {reason}"),
                )));
            }
            store.finish_commit(journal);
            #[cfg(debug_assertions)]
            if commit && store.fail_commit_saves {
                return Err(SaveError::NotWritten(std::io::Error::other(
                    "a test makes commit saves fail",
                )));
            }
            store.save(at)
        });
        let error = match saved {
            Ok(_) => return Ok(()),
            // The file may hold the commit: the tree keeps it, as the
            // renamed file's content.
            Err(SaveError::Unknown(e)) => return Err(SaveError::Unknown(e)),
            Err(SaveError::NotWritten(e)) => e,
        };
        self.roll_back(journal);
        let (dropped, kept) = self.drop_unwritable_indexes();
        if !dropped.is_empty() {
            error!(
                "{}: took out field indexes the file cannot hold, which the next find that \
                 reads them builds again: {}",
                self.name,
                describe(&dropped)
            );
        }
        if !kept.is_empty() {
            error!(
                "{}: the file cannot hold what the tree holds here, which is kept as it is \
                 and not saved: {}",
                self.name,
                describe(&kept)
            );
        }
        Err(SaveError::NotWritten(error))
    }

    /// After a save failed: take out of the tree the field indexes the file
    /// cannot hold (`om_hbam::writer::unwritable_in_tree`), each whole
    /// (`[T, 01, F]`, a field of base table `T`). An index is built from the
    /// records (query-program.md 7.1; the next find that reads it builds it
    /// again), so taking it out loses nothing. Nothing else is taken out:
    /// where any other node the file cannot hold came from is not known,
    /// and the checks where writes come in keep such nodes out, so it stays
    /// as it is and saves keep failing rather than delete data. A failure
    /// of the disk leaves nothing to take out. A read-only store takes
    /// nothing out: it loaded what it holds from the file. Returns the
    /// indexes taken out (each once) and what stays.
    pub fn drop_unwritable_indexes(&mut self) -> (Vec<Unwritable>, Vec<Unwritable>) {
        let (mut dropped, mut kept): (Vec<Unwritable>, Vec<Unwritable>) = (Vec::new(), Vec::new());
        if self.read_only.is_some() {
            return (dropped, kept);
        }
        loop {
            kept.clear();
            let mut removed = false;
            for mut node in unwritable_in_tree(&self.root, 64) {
                if !self.is_field_index(&node.path) {
                    kept.push(node);
                    continue;
                }
                node.path.truncate(3);
                let segments: Vec<&[u8]> = node.path.iter().map(Vec::as_slice).collect();
                if self.root.remove(&segments).is_some() {
                    removed = true;
                    if !dropped.iter().any(|d| d.path == node.path) {
                        dropped.push(node);
                    }
                }
            }
            if !removed {
                break;
            }
        }
        if !dropped.is_empty() {
            self.dirty = true;
        }
        (dropped, kept)
    }

    /// Whether `path` lies in a field index: `[T, 01, F, ..]` with `T` a base
    /// table and `F` one of its fields (query-program.md 7.1).
    fn is_field_index(&self, path: &[Vec<u8>]) -> bool {
        match path {
            [table, indexes, field, ..] => {
                indexes.as_slice() == [keys::INDEXES]
                    && be_uint(table)
                        .and_then(|t| self.derived.catalog().table(t))
                        .is_some_and(|t| be_uint(field).and_then(|f| t.field(f)).is_some())
            }
            _ => false,
        }
    }

    /// Run `work` on a commit's changes: when it panics, everything the
    /// commit changed is put back before the panic goes on, so the store a
    /// poisoned lock hands to the next request is the one before the commit
    /// ([`crate::hosting::Hosted::write_store`]).
    pub fn guarded<T>(
        &mut self,
        journal: &mut Journal,
        work: impl FnOnce(&mut Store, &mut Journal) -> T,
    ) -> T {
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(self, journal)));
        match done {
            Ok(value) => value,
            Err(panic) => {
                let taken = std::mem::replace(journal, self.begin_commit());
                self.roll_back(taken);
                // Begun again from the store as it was put back, so that a
                // handler outside, rolling this one back, leaves it so.
                *journal = self.begin_commit();
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// Put back everything a commit changed, newest first.
    pub(crate) fn roll_back(&mut self, journal: Journal) {
        // The record lists and counts are nodes the journal kept; a
        // record's own undo only moves it back in the indexes.
        for undo in journal.undo.into_iter().rev() {
            match undo {
                Undo::Record(table, id, old) => {
                    let current = self
                        .node(&[&table, &[keys::RECORDS], &serial::encode(id)])
                        .cloned();
                    // The write went through these nodes, so its reverse can.
                    if let Err(reason) = self.put_record(
                        &table,
                        id,
                        current.as_ref(),
                        old,
                        None,
                        &mut BTreeSet::new(),
                        false,
                    ) {
                        error!(
                            "{}: record {id} of table {table:02x?} could not be put back: {reason}",
                            self.name
                        );
                    }
                }
                Undo::Node(path, old) => {
                    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
                    match old {
                        Some(node) => self.root.put(&segments, node),
                        None => {
                            self.root.remove(&segments);
                        }
                    }
                }
            }
        }
        if let Some(derived) = journal.schema {
            self.derived = derived;
        }
        for (table, basis) in journal.basis.into_iter().rev() {
            match basis {
                Some(basis) => self.basis.insert(table, basis),
                None => self.basis.remove(&table),
            };
        }
        // Catalog members put back are a change readers of them must see,
        // as their replacement was (a server script session's schema).
        self.generation += 1;
        // The accounts are those of the tree put back: the derived state
        // put back holds the ones from before the change, decoded before
        // its first change of them; one that kept none never had its
        // accounts replaced (`Store::follow_accounts`).
        self.accounts_at = self.generation;
        self.dirty = journal.was_dirty;
        self.root.push = journal.root_push;
    }

    /// The file's schema as last read.
    pub fn catalog(&self) -> &Catalog {
        self.derived.catalog()
    }

    /// The state readers need besides the tree ([`Derived`]).
    pub(crate) fn derived(&self) -> &Arc<Derived> {
        &self.derived
    }

    /// The tree as a view (`engine::TreeView`), at the store's version.
    pub(crate) fn tree_view(&self) -> TreeView<'_> {
        TreeView::new(&self.root, self.version)
    }

    /// What reads left waiting for a write ([`Waiting`]).
    pub(crate) fn waiting(&self) -> &Waiting {
        &self.waiting
    }

    /// The store is being taken for writing: views from now on read a new
    /// version.
    pub(crate) fn begin_writing(&mut self) {
        self.version += 1;
    }

    /// The order of the parts of the file's stored dates.
    pub fn date_order(&self) -> DateOrder {
        self.derived.date_order()
    }

    /// How the file's stored numbers and dates are read for their index
    /// keys ([`Derived::reading`]).
    fn reading(&self) -> Reading {
        self.derived.reading()
    }

    /// The node at `path`, if present.
    pub fn node(&self, path: &[&[u8]]) -> Option<&OwnedNode> {
        self.root.at(path)
    }

    /// Whether `table` (a one-segment key) is a base table of the file.
    pub fn has_table(&self, table: &[u8]) -> bool {
        self.derived.has_table(table)
    }

    /// The data type of `field` in `table`, if the field exists.
    pub fn field_type(&self, table: &[u8], field: &[u8]) -> Option<DataType> {
        self.derived.field_type(table, field)
    }

    /// How many repetitions `field` of `table` has (1 when unknown).
    fn repetitions(&self, table: &[u8], field: &[u8]) -> u16 {
        self.derived.repetitions(table, field)
    }

    /// The repetitions `field` of `table`'s definition gives, if it reads.
    fn defined_repetitions(&self, table: &[u8], field: &[u8]) -> Option<u16> {
        self.derived.defined_repetitions(table, field)
    }

    /// The value-index keys of `text` in `field` of `table`, a field of
    /// `data_type`, as this file's indexes hold them
    /// ([`Derived::value_keys`]).
    pub fn value_keys(
        &self,
        table: &[u8],
        field: &[u8],
        data_type: DataType,
        text: &str,
    ) -> Vec<Vec<u8>> {
        self.derived.value_keys(table, field, data_type, text)
    }

    /// How `field` of `table` keys text in its indexes
    /// ([`Derived::text_keys`]).
    fn text_keys(&self, table: &[u8], field: &[u8]) -> Option<TextKeys> {
        self.derived.text_keys(table, field)
    }

    /// The form `field` of `table`, of `data_type`, is written in when its
    /// index is built and stored ([`derived::written_form`]).
    fn written_form(&self, table: &[u8], field: &[u8], data_type: DataType) -> Option<TextKeys> {
        derived::written_form(&self.tree_view(), &self.derived, table, field, data_type)
            .expect("the file engine's tree never fails a read")
    }

    /// The last record id issued in `table` (`[T, 00]`, 0 when absent).
    pub fn last_record_id(&self, table: &[u8]) -> u64 {
        self.node(&[table, &[keys::LAST_RECORD_ID]])
            .and_then(|n| n.value.as_deref())
            .and_then(|v| v.split_first())
            .filter(|(&len, segment)| usize::from(len) == segment.len())
            .and_then(|(_, segment)| serial::decode(segment))
            .unwrap_or(0)
    }

    /// Reserve `count` record ids in `table` (op71): returns the first and
    /// stores the new last issued id, its old node kept in `journal`.
    /// `None`, changing nothing, for a count of 0, when the ids would pass
    /// the largest a serial key holds (the last issued id comes from the
    /// file, and a catalog upload can set it: ids must not wrap around to
    /// those records have), and when the table's node holds a value, which
    /// the id cannot go beneath.
    pub fn reserve_record_ids(
        &mut self,
        journal: &mut Journal,
        table: &[u8],
        count: u32,
    ) -> Option<u64> {
        // The count and the arithmetic first, from the values alone; then
        // the write against the tree as it is. Neither changes anything, so
        // either refusal leaves the table as it was.
        if count == 0 {
            return None;
        }
        let last = self.last_record_id(table);
        let Some(new_last) = last.checked_add(u64::from(count)) else {
            warn!(
                "{}: no record ids reserved: {count} do not fit after the last issued id \
                 {last} of table {table:02x?}",
                self.name
            );
            return None;
        };
        let leaf = OwnedNode::leaf(vec![keys::LAST_RECORD_ID], vec![0; 10]);
        let refused = put_refusals(&self.root, &[table], &leaf, 1);
        if !refused.is_empty() {
            warn!(
                "{}: no record ids reserved: {}",
                self.name,
                describe(&refused)
            );
            return None;
        }
        let segment = serial::encode(new_last);
        let mut value = vec![segment.len() as u8];
        value.extend(segment);
        journal.save_path(&self.root, vec![table.to_vec(), vec![keys::LAST_RECORD_ID]]);
        self.root.put(
            &[table, &[keys::LAST_RECORD_ID]],
            OwnedNode::leaf(Vec::new(), value),
        );
        self.dirty = true;
        // At most `new_last`, as `count` is at least 1.
        Some(last + 1)
    }

    /// A field's next serial value (op72), read and possibly replaced or
    /// advanced; returns the value before the update. `None` when the field
    /// does not exist, or when the new value cannot go beneath its
    /// definition as the tree is (a definition that holds a value), which
    /// changes nothing.
    pub fn serial_value(
        &mut self,
        table: &[u8],
        field: &[u8],
        replace: Option<&str>,
        increment: Option<i32>,
    ) -> Option<String> {
        self.field_type(table, field)?;
        let path: [&[u8]; 5] = [
            table,
            &[keys::FIELDS],
            &[keys::FIELD_DEFINITIONS],
            field,
            &[keys::SERIAL_VALUE],
        ];
        let current = self
            .node(&path)
            .and_then(cell_text)
            .unwrap_or_else(|| "1".to_string());
        let next = match (replace, increment) {
            (Some(value), _) => Some(value.to_string()),
            (None, Some(by)) => Some(increment_serial(&current, by)),
            (None, None) => None,
        };
        if let Some(next) = next {
            // Only an existing field definition gets a serial node, and only
            // one the tree can take it under.
            if self.node(&path[..4]).is_some() {
                let leaf = OwnedNode::leaf(vec![keys::SERIAL_VALUE], om_scsu::encode_scsu(&next));
                let refused = put_refusals(&self.root, &path[..4], &leaf, 1);
                if !refused.is_empty() {
                    warn!(
                        "{}: serial value not set: {}",
                        self.name,
                        describe(&refused)
                    );
                    return None;
                }
                self.root.put(
                    &path,
                    OwnedNode::leaf(Vec::new(), om_scsu::encode_scsu(&next)),
                );
                self.dirty = true;
            }
        }
        Some(current)
    }

    /// [`Store::serial_value`] (op72), its serial node as it was kept in
    /// `journal` when it changes.
    pub fn serial_value_in(
        &mut self,
        journal: &mut Journal,
        table: &[u8],
        field: &[u8],
        replace: Option<&str>,
        increment: Option<i32>,
    ) -> Option<String> {
        let path: Vec<Vec<u8>> = vec![
            table.to_vec(),
            vec![keys::FIELDS],
            vec![keys::FIELD_DEFINITIONS],
            field.to_vec(),
            vec![keys::SERIAL_VALUE],
        ];
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let old = self.node(&segments).cloned();
        let current = self.serial_value(table, field, replace, increment)?;
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        if self.node(&segments) != old.as_ref() && journal.saved.insert(path.clone()) {
            // Only an existing field definition gets a serial node, so its
            // parents were there.
            journal.undo.push(Undo::Node(path, old));
        }
        Some(current)
    }

    /// op88: store binary data in the library at `path` (`[T, 1f]` or
    /// `[1f]`), creating the library on first use (containers.md 3.4).
    #[cfg(test)]
    pub fn store_binary(
        &mut self,
        path: &[&[u8]],
        data: BinaryData,
        unique_id: impl FnMut() -> [u8; 16],
    ) -> Option<Stored> {
        let mut journal = self.begin_commit();
        self.store_binary_in(&mut journal, path, data, unique_id)
    }

    /// [`Store::store_binary`] within `journal`, which keeps what the store
    /// changes ([`library::store_keys`]): the member that counts one more
    /// reference, or the header and the new member, or, for a new library,
    /// the library's absence.
    pub fn store_binary_in(
        &mut self,
        journal: &mut Journal,
        path: &[&[u8]],
        data: BinaryData,
        unique_id: impl FnMut() -> [u8; 16],
    ) -> Option<Stored> {
        if let Some(reason) = self.library_refusal(path, Some(&data), None) {
            warn!("{}: container data not stored: {reason}", self.name);
            return None;
        }
        let base: Vec<Vec<u8>> = path.iter().map(|s| s.to_vec()).collect();
        // One decision, which the journal keeps and the store carries out.
        let plan = library::store_plan(self.node(path), &data.md5);
        match plan.keys(self.node(path)) {
            None => journal.save_path(&self.root, base),
            Some(keys) => {
                for keys in keys {
                    let mut at = base.clone();
                    at.extend(keys);
                    journal.save_path(&self.root, at);
                }
            }
        }
        let stored = library::store_as(self.root.ensure(path), data, &plan, unique_id)?;
        self.dirty = true;
        Some(stored)
    }

    /// Why a store of `data` into the library at `path` (or, with `member`,
    /// an adjustment of that member's count by its delta) would change a
    /// value it does not name, add what the file cannot hold, or advance a
    /// counter past the largest a serial key holds: a node on the library's
    /// path, its header, lists, members or the member that holds a value
    /// ([`library::store_refusal`], [`library::adjust_refusal`]), a last
    /// member id or modification count the file holds at the largest
    /// ([`library::store_counter_refusal`],
    /// [`library::adjust_counter_refusal`]), or an uploaded key the new
    /// member could not hold (`om_hbam::writer::unwritable`). For a log
    /// line; `None` when the write is safe.
    fn library_refusal(
        &self,
        path: &[&[u8]],
        data: Option<&BinaryData>,
        member: Option<(&[u8], i32)>,
    ) -> Option<String> {
        if let Some(found) = value_on_path(&self.root, path) {
            return Some(describe_value(&found));
        }
        let library = self.node(path);
        let inner = match (member, data) {
            (Some((member, _)), _) => library.and_then(|l| library::adjust_refusal(l, member)),
            (None, Some(data)) => library.and_then(|l| library::store_refusal(l, &data.md5)),
            (None, None) => None,
        };
        if let Some(inner) = inner {
            let mut found: Vec<Vec<u8>> = path.iter().map(|s| s.to_vec()).collect();
            found.extend(inner);
            return Some(describe_value(&found));
        }
        let counters = match (member, data) {
            (Some((member, delta)), _) => {
                library.and_then(|l| library::adjust_counter_refusal(l, member, delta))
            }
            (None, Some(data)) => {
                library.and_then(|l| library::store_counter_refusal(l, &data.md5))
            }
            (None, None) => None,
        };
        if let Some(reason) = counters {
            return Some(format!("{path:02x?}: {reason}"));
        }
        let data = data?;
        if library.is_some_and(|l| {
            library::reused_member(l, &data.md5)
                .is_some_and(|existing| library::member(l, &existing).is_some())
        }) {
            return None;
        }
        let Some(new_member) = library::next_member(library) else {
            return Some(format!("{path:02x?}: no member id is left"));
        };
        let mut parents = path.to_vec();
        parents.push(&[library::MEMBERS]);
        parents.push(&new_member);
        data.keys
            .iter()
            .map(|key| unwritable(&parents, key, 3))
            .find(|refused| !refused.is_empty())
            .map(|refused| describe(&refused))
    }

    /// op87: add `delta` to a member's reference count in the library at
    /// `path`, deleting it at 0 (containers.md 5.3). `None` when there is no
    /// such member.
    #[cfg(test)]
    pub fn adjust_reference_count(
        &mut self,
        path: &[&[u8]],
        member: &[u8],
        delta: i32,
    ) -> Option<u32> {
        let mut journal = self.begin_commit();
        self.adjust_reference_count_in(&mut journal, path, member, delta)
    }

    /// [`Store::adjust_reference_count`] within `journal`, which keeps the
    /// member's count keys, or the member and the header when it goes.
    pub fn adjust_reference_count_in(
        &mut self,
        journal: &mut Journal,
        path: &[&[u8]],
        member: &[u8],
        delta: i32,
    ) -> Option<u32> {
        if let Some(reason) = self.library_refusal(path, None, Some((member, delta))) {
            warn!("{}: reference count not changed: {reason}", self.name);
            return None;
        }
        // The decision the adjustment makes; none for a delta of 0.
        let changed = library::adjust_keys(self.node(path)?, member, delta)?;
        let base: Vec<Vec<u8>> = path.iter().map(|s| s.to_vec()).collect();
        for keys in changed {
            let mut at = base.clone();
            at.extend(keys);
            journal.save_node(&self.root, at);
        }
        let count = library::adjust(self.root.at_mut(path)?, member, delta)?;
        self.dirty |= delta != 0;
        Some(count)
    }

    /// A member of the library at `path`.
    pub fn library_member(&self, path: &[&[u8]], member: &[u8]) -> Option<&OwnedNode> {
        library::member(self.node(path)?, member)
    }

    /// The member of `table`'s library a container field's stored text
    /// names (containers.md 1.4), and its key.
    fn named_member(&self, table: &[u8], text: &str) -> Option<(Vec<u8>, &OwnedNode)> {
        let member = containers::reference(text)?;
        let node = self.library_member(&[table, &[LIBRARY]], &member)?;
        Some((member, node))
    }

    /// A container field's value of `table` as calculations see it, from
    /// its stored text: the file name of the library member it names, the
    /// member referenced for its data ([`Store::member_data`]), which is
    /// read only when a function needs it. Text that names no member of the
    /// library (a reference the library lost) stays the reference alone,
    /// without data.
    pub fn container(&self, table: &[u8], text: &str) -> om_calc::Container {
        let table_id = be_uint(table).unwrap_or(0);
        self.named_member(table, text)
            .and_then(|(member, node)| Some(MemberInfo::read(node)?.value(table_id, &member)))
            .unwrap_or_else(|| om_calc::Container {
                text: om_calc::Text::plain(text),
                ..Default::default()
            })
    }

    /// The data of member `member` of `table`'s library ([`containers::read`]:
    /// every stream, whether they are intact).
    pub fn member_data(
        &self,
        table: &[u8],
        member: &[u8],
    ) -> Option<om_calc::eval::StoredContainer> {
        containers::read(self.library_member(&[table, &[LIBRARY]], member)?)
    }

    /// The text form of a container field's value: the file name of the
    /// member its stored text names (execute-sql.md 8.1), or the text
    /// itself when it names none.
    pub fn container_name(&self, table: &[u8], text: &str) -> String {
        match self.named_member(table, text) {
            Some((_, member)) => MemberInfo::read(member).map(|i| i.name).unwrap_or_default(),
            None => text.to_string(),
        }
    }

    /// What became of the member keys a request's GenerateMemberKey issued
    /// (each catalog with its id), asked once the request submitted its
    /// own write ([`derived::issued_keys`], read through the store's tree).
    pub fn issued_keys(&self, issued: &[(Vec<Vec<u8>>, u64)]) -> IssuedKeysAre<'_> {
        derived::issued_keys(&self.tree_view(), self.writes_refused(), issued)
            .expect("the file engine's tree never fails a read")
    }

    /// Issue the next member id and modification count of the catalog at
    /// `catalog` ([`derived::issue_member_key`]); [`Store::write_member_keys`]
    /// stores them.
    pub fn issue_member_key(&self, catalog: &[Vec<u8>]) -> Option<(u64, u64)> {
        derived::issue_member_key(&self.tree_view(), &self.waiting, &self.name, catalog)
            .expect("the file engine's tree never fails a read")
    }

    /// Whether GenerateMemberKey issued ids not yet written.
    pub fn has_issued_member_keys(&self) -> bool {
        self.waiting.has_member_keys()
    }

    /// Take the member ids and modification counts GenerateMemberKey
    /// issued and nobody wrote yet, by catalog, for
    /// [`Store::write_member_keys`]: they are no longer waiting.
    pub fn take_issued_member_keys(&mut self) -> Vec<MemberKey> {
        self.waiting.take_member_keys()
    }

    /// Put `keys`, taken by [`Store::take_issued_member_keys`] and not
    /// written, back among those waiting: a write that took them was left
    /// by a panic before it wrote them.
    pub fn put_back_member_keys(&mut self, keys: Vec<MemberKey>) {
        self.waiting.put_back_member_keys(keys);
    }

    /// Write member ids and modification counts GenerateMemberKey issued
    /// ([`Store::take_issued_member_keys`]) into their catalogs' indexes, as
    /// FileMaker Server does (schema-path.md 3.4). A value the index
    /// already passed stays.
    #[cfg(test)]
    pub fn write_member_keys(&mut self, keys: Vec<MemberKey>) {
        let mut journal = self.begin_commit();
        self.write_member_keys_in(&mut journal, keys);
    }

    /// [`Store::write_member_keys`] within `journal`, which keeps each
    /// index key it advances.
    pub fn write_member_keys_in(&mut self, journal: &mut Journal, keys: Vec<MemberKey>) {
        for (catalog, (id, count)) in keys {
            let mut path: Vec<&[u8]> = catalog.iter().map(Vec::as_slice).collect();
            // Checked again here: the tree may have changed since the key
            // was issued.
            if let Some(reason) = self.member_key_refusal(&path) {
                warn!(
                    "{}: member keys issued for {path:02x?} are not written: {reason}",
                    self.name
                );
                continue;
            }
            path.push(&[CATALOG_INDEX]);
            // Each key the index has passed already stays; one that
            // advances is kept in the journal, with the index it creates,
            // as it is changed.
            for (key, value) in [(LAST_MEMBER_ID, id), (MOD_COUNT, count)] {
                let current = self.root.at(&path).and_then(|index| index.child(&[key]));
                if modcount::serial_value(current) < value {
                    let mut at: Vec<Vec<u8>> = path.iter().map(|s| s.to_vec()).collect();
                    at.push(vec![key]);
                    journal.save_path(&self.root, at);
                    self.root
                        .ensure(&path)
                        .set_child(modcount::serial_leaf(key, value));
                }
            }
            self.dirty = true;
        }
    }

    /// Why putting `node` at `path` would change a value it does not name
    /// or leave what the file cannot hold, in the tree as it is
    /// (`put_refusals`), for a log line.
    fn replacement_refusal(&self, path: &[Vec<u8>], node: &OwnedNode) -> Option<String> {
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let (last, parents) = segments.split_last()?;
        let placed = OwnedNode {
            segment: last.to_vec(),
            ..node.clone()
        };
        let refused = put_refusals(&self.root, parents, &placed, 3);
        (!refused.is_empty()).then(|| describe(&refused))
    }

    /// Why `[catalog, 01, 00]` and `[catalog, 01, fc]` (a catalog's last
    /// member id and count, schema-path.md 3.4) cannot be written in the
    /// tree as it is (`put_refusals`), for a log line.
    fn member_key_refusal(&self, catalog: &[&[u8]]) -> Option<String> {
        derived::member_key_refusal(&self.tree_view(), catalog)
            .expect("the file engine's tree never fails a read")
    }

    /// Apply the changes of an op86 program's schema instructions
    /// (UploadMultiple, DeleteTable; schema-path.md 3.4) and re-read the
    /// schema. Returns the libraries a deleted table took with it: its
    /// container data goes with `[T]` (`[T, 1f]`, containers.md 1.1), every
    /// member at once, so no reference is left to release; the caller
    /// forgets what it derived from them (thumbnails).
    #[cfg(test)]
    pub fn apply(&mut self, writes: Vec<Write>) -> Vec<Vec<Vec<u8>>> {
        let mut journal = self.begin_commit();
        self.apply_in(&mut journal, writes)
    }

    /// [`Store::apply`] within `journal`, which keeps what each write
    /// replaces or removes (moved, not copied) and the parents it creates,
    /// and reads the schema again when it is taken back.
    pub fn apply_in(&mut self, journal: &mut Journal, writes: Vec<Write>) -> Vec<Vec<Vec<u8>>> {
        let mut released = Vec::new();
        if writes.is_empty() {
            return released;
        }
        journal.schema_from(&self.derived);
        for write in writes {
            match write {
                Write::Put { path: owned, node } => {
                    let path: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
                    let Some((last, parents)) = path.split_last() else {
                        warn!("{}: refusing a write over the whole file", self.name);
                        continue;
                    };
                    let placed = OwnedNode {
                        segment: last.to_vec(),
                        ..node
                    };
                    let refused = put_refusals(&self.root, parents, &placed, 3);
                    if !refused.is_empty() {
                        // om-query refuses these before they get here.
                        warn!(
                            "{}: refusing a write to {path:02x?} the file cannot hold: {}",
                            self.name,
                            describe(&refused)
                        );
                        continue;
                    }
                    journal.replace(&mut self.root, owned, placed);
                }
                Write::Remove { path } => {
                    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
                    if let [table] = segments.as_slice() {
                        let members = self
                            .node(&[table, &[LIBRARY], &[containers::keys::MEMBERS]])
                            .map_or(0, |m| m.children.len());
                        if members > 0 {
                            note(
                                &self.name,
                                Some(journal),
                                format!(
                                    "table {table:02x?} deleted with its {members} container \
                                     library member(s)"
                                ),
                            );
                        }
                        released.push(vec![table.to_vec(), vec![LIBRARY]]);
                    }
                    journal.take(&mut self.root, path);
                }
            }
        }
        self.schema_changed();
        released
    }

    /// Replace catalog members within a commit ([`Store::begin_commit`]):
    /// each node's old version goes into `journal`, so a failed save puts
    /// it back ([`Store::save_commit`]). For catalogs the schema
    /// ([`Store::catalog`]) does not hold, such as the scripts `[17]`;
    /// server script sessions read them again
    /// ([`Store::schema_generation`]).
    pub fn replace_members_in(
        &mut self,
        journal: &mut Journal,
        members: Vec<(Vec<Vec<u8>>, OwnedNode)>,
    ) {
        for (path, node) in members {
            if let Some(reason) = self.replacement_refusal(&path, &node) {
                journal.refused.get_or_insert(reason);
                continue;
            }
            journal.save_path(&self.root, path.clone());
            let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            self.root.put(&segments, node);
        }
        self.dirty = true;
        self.generation += 1;
    }

    /// Replace the schema catalog at `path`, such as a table's fields
    /// `[T, 03]`, within a commit ([`Store::begin_commit`]) and read the
    /// schema again, so writes and queries see the change; a failed save
    /// puts the catalog back and reads the schema again
    /// ([`Store::save_commit`]).
    pub fn replace_schema_in(
        &mut self,
        journal: &mut Journal,
        path: Vec<Vec<u8>>,
        node: OwnedNode,
    ) {
        if let Some(reason) = self.replacement_refusal(&path, &node) {
            journal.refused.get_or_insert(reason);
            return;
        }
        journal.save_path(&self.root, path.clone());
        journal.schema_from(&self.derived);
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        self.root.put(&segments, node);
        self.schema_changed();
    }

    /// Replace the node at `path` within a commit ([`Store::begin_commit`]):
    /// a node no record or schema reader holds, such as a table
    /// occurrence's saved found set `[TO, 07]`. A failed save puts it back
    /// ([`Store::save_commit`]).
    pub fn replace_node_in(&mut self, journal: &mut Journal, path: Vec<Vec<u8>>, node: OwnedNode) {
        if let Some(reason) = self.replacement_refusal(&path, &node) {
            journal.refused.get_or_insert(reason);
            return;
        }
        journal.save_path(&self.root, path.clone());
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        self.root.put(&segments, node);
        self.dirty = true;
        self.generation += 1;
    }

    /// Remove the schema node at `path` (a layout member Delete Layout
    /// removes) in `journal`'s commit, which puts it back if the file is not
    /// written.
    pub fn remove_schema_in(&mut self, journal: &mut Journal, path: Vec<Vec<u8>>) {
        journal.save_node(&self.root, path.clone());
        journal.schema_from(&self.derived);
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        self.root.remove(&segments);
        self.schema_changed();
    }

    /// Count field `field` of `table`, which was just added, as computed
    /// with its definition ([`recalc`]): a new field has no stored values
    /// to recompute.
    pub fn add_field_basis(&mut self, table: &[u8], field: u64) {
        let Some(definition) = be_uint(table)
            .and_then(|t| self.derived.catalog().table(t))
            .cloned()
        else {
            return;
        };
        let Some(entry) = recalc::table_basis(&self.root, &definition).remove(&field) else {
            return;
        };
        if let Some(basis) = self.basis.get_mut(table) {
            basis.insert(field, entry);
        }
    }

    /// op13: replace the subtree at `path` with `node` (schema-path.md 3.3),
    /// creating missing parents, and re-read the schema. A subtree the file
    /// cannot hold there (hbam-writer.md 2.2, 2.3) is refused and logged, as
    /// op13 refuses an upload it cannot store, and the tree is unchanged.
    /// Returns whether it was stored.
    pub fn put_subtree(&mut self, path: &[Vec<u8>], node: OwnedNode) -> bool {
        let mut journal = self.begin_commit();
        self.put_subtree_in(&mut journal, path, node)
    }

    /// [`Store::put_subtree`] within `journal`, which keeps the subtree it
    /// replaces (moved, not copied) and the parents it creates, and reads
    /// the schema again when it is taken back.
    pub fn put_subtree_in(
        &mut self,
        journal: &mut Journal,
        path: &[Vec<u8>],
        node: OwnedNode,
    ) -> bool {
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let Some((last, parents)) = segments.split_last() else {
            warn!("{}: refusing an upload over the whole file", self.name);
            return false;
        };
        let placed = OwnedNode {
            segment: last.to_vec(),
            ..node
        };
        let refused = put_refusals(&self.root, parents, &placed, 3);
        if !refused.is_empty() {
            warn!(
                "{}: refusing an upload to {segments:02x?} the file cannot hold: {}",
                self.name,
                describe(&refused)
            );
            return false;
        }
        journal.replace(&mut self.root, path.to_vec(), placed);
        journal.schema_from(&self.derived);
        self.schema_changed();
        true
    }

    /// Re-read the schema catalogs after the client changed them, so writes
    /// and queries see new tables and fields, and tell those who read them
    /// earlier ([`Store::schema_generation`]).
    fn schema_changed(&mut self) {
        self.dirty = true;
        self.generation += 1;
        self.reread_catalog();
    }

    fn reread_catalog(&mut self) {
        match derived::read_catalog(&self.tree_view()) {
            Ok(catalog) => {
                self.derived = Arc::new(self.derived.with_catalog(self.version, catalog));
            }
            Err(e) => warn!("re-reading the schema after a change: {e}"),
        }
    }

    /// Decode the file's accounts again ([`derived::read_accounts`]) when
    /// the schema generation moved since they were: a change of the
    /// security catalog moves it, as every schema change does, and the
    /// accounts are decoded again at the end of the operation that moved
    /// it, before its save, as the store's write lock is still held
    /// (`FileEngine::submit`; `hosting::StoreWriteGuard` for a write
    /// outside an operation). The derived state that held the accounts is
    /// kept in `journal` first, so that a roll-back puts back the accounts
    /// from before the operation with the rest of it ([`Store::roll_back`]).
    pub(crate) fn follow_accounts(&mut self, journal: Option<&mut Journal>) {
        if self.accounts_at == self.generation {
            return;
        }
        if let Some(journal) = journal {
            journal.schema_from(&self.derived);
        }
        let accounts = derived::read_accounts(&self.tree_view());
        self.derived = Arc::new(self.derived.with_accounts(self.version, accounts));
        self.accounts_at = self.generation;
    }

    /// How many times the schema changed while the file was hosted: a
    /// server script session that read the schema (record rules, the SQL
    /// catalog, layouts, scripts) at another count reads it again before
    /// it uses it. op73's field work counts too, since it changes stored
    /// values and indexes under records a session may hold.
    pub fn schema_generation(&self) -> u64 {
        self.generation
    }

    /// op73: do the field work the client listed in `[T, 03, 01, fd]` and
    /// clear the list (schema-path.md 7.2): drop the values of deleted
    /// fields (releasing container data), recompute the stored calculations
    /// the change made stale ([`recalc`]; `environment` gives what `Get`
    /// answers, `unique_id` the unique ids of container data a calculation
    /// made), and delete or build field indexes. A field defined as
    /// indexed that has no index yet gets one too: how FileMaker Pro has a
    /// new table's indexes built is not established (schema-path.md 12.1).
    /// Everything happens in memory under the caller's write lock; the
    /// caller's one save then writes it all or, if the server stops first,
    /// nothing (the work list stays in the file for [`Store::recover`]).
    /// With a `journal` (op73) every change goes into it, so a save that
    /// fails takes it all back: each record as it was before its first
    /// change, the work list, global values, indexes and library entries
    /// replaced or removed, and the table's basis; the log lines that say
    /// what changed wait in it too. Without one (a test's) the changes are
    /// made in place.
    /// Returns whether anything changed.
    pub fn update_table(
        &mut self,
        mut journal: Option<&mut Journal>,
        table: &[u8],
        environment: &mut dyn FnMut() -> RecordEnvironment,
        unique_id: &mut dyn FnMut() -> [u8; 16],
    ) -> Result<bool, &'static str> {
        if self.is_read_only() {
            return Err("the file is hosted read-only");
        }
        let Some(definition) = be_uint(table)
            .and_then(|t| self.derived.catalog().table(t))
            .cloned()
        else {
            return Ok(false);
        };
        let preflight = (|| {
            let work = self.deferred_work(table)?;
            let basis = recalc::table_basis(&self.root, &definition);
            let retyped = recalc::retyped_to_container(self.basis.get(table), &basis)?;
            Ok((work, basis, retyped))
        })();
        let (work, basis, retyped) = match preflight {
            Ok(prepared) => prepared,
            Err(reason) => {
                warn!(
                    "{}: table {table:02x?}: deferred work kept, hosting read-only: {reason}",
                    self.name
                );
                self.set_read_only(ReadOnlyCause::DeferredWork);
                return Err(reason);
            }
        };
        let stale = recalc::changed_fields(self.basis.get(table), &basis, &work);
        let list = Self::deferred_path(table);
        let mut changed = match journal.as_deref_mut() {
            Some(journal) => {
                let list = list.iter().map(|s| s.to_vec()).collect();
                journal.take(&mut self.root, list)
            }
            None => self.root.remove(&list).is_some(),
        };
        // Every field the work names keeps its own cells: a field deleted in
        // the same edit is no longer in the catalog, and its key may read as
        // another's repetition (`repetition_cells`).
        let mut named: BTreeSet<Vec<u8>> = work.iter().map(|(field, _)| field.clone()).collect();
        if let Some(old) = self.basis.get(table) {
            named.extend(old.keys().map(|id| uint_segment(*id)));
        }
        let mut removed = BTreeMap::new();
        for (field, bits) in &work {
            if bits & deferred::REMOVES_VALUES != 0 {
                // For a repetition cut, the preflight checked the new
                // maximum. A delete or type change removes all repetitions.
                let from = if bits & deferred::FEWER != 0 && bits & deferred::DELETE == 0 {
                    self.repetitions(table, field).saturating_add(1)
                } else {
                    1
                };
                let container = bits & deferred::CONTAINER != 0;
                let values = (from, container);
                if bits & deferred::GLOBAL != 0 {
                    let journal = journal.as_deref_mut();
                    self.drop_global_value(journal, table, field, values, &named, &mut removed);
                } else {
                    let journal = journal.as_deref_mut();
                    self.drop_field_values(journal, table, field, values, &named, &mut removed);
                }
                changed = true;
            }
            // The values left are rewritten as dates or timestamps, after
            // the repetitions above a reduced maximum went (7.2, step 3;
            // 7.2.1): no deleted value, no container's, and not a global's,
            // which is not in the records the rewrite walks.
            let kept = bits & (deferred::DELETE | deferred::CONTAINER | deferred::GLOBAL) == 0;
            if kept && bits & (deferred::TO_DATE | deferred::TO_TIMESTAMP) != 0 {
                let timestamp = bits & deferred::TO_TIMESTAMP != 0;
                let journal = journal.as_deref_mut();
                changed |= self.convert_field_values(
                    journal,
                    table,
                    field,
                    timestamp,
                    &named,
                    environment,
                );
            }
            if bits & (deferred::DELETE | deferred::DROP_INDEX) != 0 {
                let index = [table, &[keys::INDEXES], field.as_slice()];
                changed |= match journal.as_deref_mut() {
                    Some(journal) => {
                        let index = index.iter().map(|s| s.to_vec()).collect();
                        journal.take(&mut self.root, index)
                    }
                    None => self.root.remove(&index).is_some(),
                };
            }
        }
        let before = self.basis.insert(table.to_vec(), basis);
        if let Some(journal) = journal.as_deref_mut() {
            journal.basis.push((table.to_vec(), before));
        }
        for field in &retyped {
            let journal = journal.as_deref_mut();
            changed |= self.drop_retyped_cells(journal, table, *field, &named);
        }
        if !removed.is_empty() {
            let naming = self.naming_cells(table, &work, &named);
            self.release_containers(journal.as_deref_mut(), table, removed, &naming);
        }
        let recomputed = if stale.is_empty() {
            Vec::new()
        } else {
            self.recalculate(
                journal.as_deref_mut(),
                table,
                &definition,
                &stale,
                (&work, &named),
                (environment, unique_id),
            )
        };
        changed |= !recomputed.is_empty();
        let mut rebuild: BTreeSet<Vec<u8>> = work
            .iter()
            .filter(|(_, bits)| bits & deferred::BUILD_INDEX != 0 && bits & deferred::DELETE == 0)
            .map(|(field, _)| field.clone())
            .collect();
        // A field whose type changed is keyed anew in every index it has.
        // `0x10` names an index the definition asks for; one a query built
        // on demand (`store_indexes`) sets no option bit, so it
        // would keep the old type's keys (#441).
        rebuild.extend(
            work.iter()
                .filter(|(field, bits)| {
                    bits & deferred::CONVERT != 0
                        && bits & deferred::DELETE == 0
                        && self.node(&[table, &[keys::INDEXES], field]).is_some()
                })
                .map(|(field, _)| field.clone()),
        );
        // A recomputed field's index follows its new values.
        rebuild.extend(
            recomputed
                .iter()
                .map(|f| uint_segment(*f))
                .filter(|f| self.node(&[table, &[keys::INDEXES], f]).is_some()),
        );
        let fields: Vec<(Vec<u8>, DataType, u32)> = self
            .node(&[table, &[keys::FIELDS], &[keys::FIELD_DEFINITIONS]])
            .map(|n| {
                n.children
                    .iter()
                    .filter_map(|member| {
                        let field = definition.field(be_uint(&member.segment)?)?;
                        let options = field.options.get(8..12)?.try_into().ok()?;
                        Some((
                            member.segment.clone(),
                            field.data_type,
                            u32::from_be_bytes(options),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (field, data_type, options) in fields {
            let index = self.node(&[table, &[keys::INDEXES], &field]);
            // The indexes the field is defined with, and those it has (a
            // query may have built one on demand).
            let has = |a: u8, b: u8| {
                index.is_some_and(|i| i.child(&[a]).is_some() || i.child(&[b]).is_some())
            };
            let value =
                options & VALUE_INDEX_OPTION != 0 || has(keys::VALUE_INDEX, keys::VALUE_COUNTS);
            let word = options & WORD_INDEX_OPTION != 0 || has(keys::WORD_INDEX, keys::WORD_COUNTS);
            let missing = index.is_none();
            if rebuild.contains(&field) || (missing && (value || word)) {
                let journal = journal.as_deref_mut();
                self.build_index(journal, table, &field, data_type, (value, word));
                changed = true;
            }
        }
        self.dirty |= changed;
        if changed {
            self.generation += 1;
        }
        Ok(changed)
    }

    /// `f` of the catalogs a calculation of `table` reads and of the file's
    /// calculation schema ([`derived::with_calc_inputs`]); `None` when they
    /// cannot be read.
    fn with_calc_inputs<R>(
        &self,
        table: &[u8],
        f: impl FnOnce(&KeyTree<'_>, &om_data::host::CalcSchema) -> R,
    ) -> Option<R> {
        derived::with_calc_inputs(&self.tree_view(), &self.derived, table, f)
            .expect("the file engine's tree never fails a read")
    }

    /// The evaluator of a custom privilege set's formula `code` limiting a
    /// record action on base table `table` ([`derived::record_limit`]).
    pub(crate) fn record_limit(
        &self,
        table: &[u8],
        code: &[u8],
    ) -> Result<om_data::rules::recalc::RecordLimit, String> {
        derived::record_limit(&self.tree_view(), &self.derived, table, code)
            .expect("the file engine's tree never fails a read")
    }

    /// Recompute the stored calculations of `table` that the fields in
    /// `stale` make stale, on every record (schema-path.md "Implementation
    /// notes (recalculation)"). A calculation this server cannot evaluate,
    /// and a record on which one fails, keep their values; both are
    /// logged. Returns the calculations recomputed.
    fn recalculate(
        &mut self,
        mut journal: Option<&mut Journal>,
        table: &[u8],
        definition: &om_hbam::Table,
        stale: &BTreeSet<u64>,
        deferred: DeferredFieldWork<'_>,
        (environment, unique_id): Draws<'_>,
    ) -> Vec<u64> {
        let Some(plan) = self.with_calc_inputs(table, |tree, calc| {
            Recalculation::plan(definition, tree, calc, stale)
        }) else {
            warn!(
                "{}: table {table:02x?}: the field catalog could not be read; \
                 stored calculations were not recomputed",
                self.name
            );
            return Vec::new();
        };
        for skipped in plan.skipped() {
            warn!(
                "{}: table {:?}: stored calculation {:?} was not recomputed, its values \
                 are left as they were: {}",
                self.name, definition.name, skipped.name, skipped.reason
            );
        }
        if plan.is_empty() {
            return Vec::new();
        }
        let fields = plan.fields();
        // Calculations read container data from the table's library, set
        // aside (not copied) while the records are rewritten, and put back
        // whatever happens.
        let library = self
            .root
            .remove(&[table, &[LIBRARY]])
            .map(std::sync::Arc::new);
        let reader = library.as_ref().map(|library| {
            std::sync::Arc::new(LibraryContainers {
                table: be_uint(table).unwrap_or(0),
                library: std::sync::Arc::clone(library),
            })
        });
        let mut with_containers = || {
            let mut environment = environment();
            if let Some(reader) = &reader {
                environment.containers = Some(reader.clone());
            }
            environment
        };
        let mut before_change = |record: &OwnedNode| {
            if let Some(journal) = journal.as_deref_mut() {
                journal.save_record(table, record);
            }
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match self.root.at_mut(&[table, &[keys::RECORDS]]) {
                Some(records) => recalc::recompute_records(
                    records,
                    &plan,
                    &mut with_containers,
                    &mut before_change,
                ),
                None => recalc::Outcome::default(),
            }
        }));
        drop(reader);
        if let Some(library) = library {
            let node = std::sync::Arc::try_unwrap(library).unwrap_or_else(|l| (*l).clone());
            self.root.put(&[table, &[LIBRARY]], node);
        }
        let outcome = outcome.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        // The library is back: container results go into it, each release
        // held to the references the table's cells make
        // ([`Self::naming_cells`]).
        if !outcome.containers.is_empty() {
            let mut naming = self.naming_cells(table, deferred.0, deferred.1);
            for (record, cells) in &outcome.containers {
                self.store_recomputed_containers(
                    journal.as_deref_mut(),
                    table,
                    record,
                    cells,
                    &definition.name,
                    &mut naming,
                    unique_id,
                );
            }
        }
        for (field, (records, first)) in &outcome.failed {
            let name = definition.field(*field).map_or("", |f| f.name.as_str());
            warn!(
                "{}: table {:?}: stored calculation {name:?} failed on {records} record(s), \
                 whose values are left as they were: {first}",
                self.name, definition.name
            );
        }
        if outcome.exhausted > 0 {
            warn!(
                "{}: table {:?}: {} record(s) keep their old stored calculation values: \
                 their modification count is the largest a serial key holds",
                self.name, definition.name, outcome.exhausted
            );
        }
        let names: Vec<&str> = fields
            .iter()
            .filter_map(|f| definition.field(*f).map(|f| f.name.as_str()))
            .collect();
        note(
            &self.name,
            journal,
            format!(
                "table {:?}: recomputed {names:?} on {} record(s), {} changed",
                definition.name, outcome.records, outcome.changed
            ),
        );
        self.dirty |= outcome.changed > 0;
        fields
    }

    /// Drop, in every record of `table`, the cells of `field`, whose type
    /// changed to container in this change: they hold values of the old
    /// type, which name no member they counted, so they are not counted
    /// as references either (`recalc::retyped_to_container`). A record that
    /// lost one counts one more modification; one whose count cannot
    /// advance loses it too (it is not a container value), logged.
    /// Returns whether a record changed.
    fn drop_retyped_cells(
        &mut self,
        mut journal: Option<&mut Journal>,
        table: &[u8],
        field: u64,
        named: &BTreeSet<Vec<u8>>,
    ) -> bool {
        let key = uint_segment(field);
        let mut cells = self.repetition_cells(table, &key, named);
        cells.insert(key);
        let Some(records) = self.root.at_mut(&[table, &[keys::RECORDS]]) else {
            return false;
        };
        let mut changed = false;
        let mut exhausted = 0usize;
        for record in &mut records.children {
            if !record.children.iter().any(|c| cells.contains(&c.segment)) {
                continue;
            }
            if let Some(journal) = journal.as_deref_mut() {
                journal.save_record(table, record);
            }
            let before = record.children.len();
            record
                .children
                .retain(|cell| !cells.contains(&cell.segment));
            if record.children.len() != before {
                changed = true;
                if modcount::bump(record, 1).is_none() {
                    exhausted += 1;
                }
            }
        }
        if exhausted > 0 {
            warn!(
                "{}: table {table:02x?} field {field}: {exhausted} record(s) lost the old \
                 values of a field changed to container without counting the change: their \
                 modification count is the largest a serial key holds",
                self.name
            );
        }
        self.dirty |= changed;
        changed
    }

    /// Store the container results `cells` the recompute computed for
    /// record `record` of `table` (its key under `[T, 05]`), as a server
    /// script's commit stores container data (containers.md,
    /// "Implementation notes (OpenMaker Server scripts)"): a member of the
    /// table's library the value names takes one more reference, data
    /// (another table's member's, or data a calculation made) is stored as
    /// a member, or counts one more reference to a member with the same
    /// data, and text is the cell's text; a member a cell no longer names
    /// is released (deleted at 0). When the library would refuse a write,
    /// the record keeps its container values, logged.
    ///
    /// `naming` tracks surviving cells, including retained text in the
    /// form of a member reference. Releases cannot spend the references
    /// reserved for those cells or for this record's new results.
    /// `naming` follows the record's change only once storage succeeds.
    #[allow(clippy::too_many_arguments)]
    fn store_recomputed_containers(
        &mut self,
        outer: Option<&mut Journal>,
        table: &[u8],
        record: &[u8],
        cells: &BTreeMap<u64, ContainerResult>,
        name: &str,
        naming: &mut BTreeMap<Vec<u8>, i64>,
        unique_id: &mut dyn FnMut() -> [u8; 16],
    ) {
        let path: [&[u8]; 3] = [table, &[keys::RECORDS], record];
        let Some(old) = self.node(&path).cloned() else {
            return;
        };
        let mut new = old.clone();
        let mut journal = self.begin_commit();
        // Added cells, removed names, and additions already counted by storage.
        let mut references: BTreeMap<Vec<u8>, (i32, i32, i32)> = BTreeMap::new();
        for (cell, result) in cells {
            let key = uint_segment(*cell);
            if let Some(member) = old.child(&key).and_then(member_key) {
                references.entry(member).or_default().1 += 1;
            }
            let text = match result {
                ContainerResult::Empty => None,
                ContainerResult::Text(text) => Some(text.clone()),
                ContainerResult::Data(value) => {
                    let Some((member, stored)) =
                        self.recomputed_member(&mut journal, table, value, unique_id)
                    else {
                        journal.refused.get_or_insert_with(|| {
                            "the container value has no data to store".to_string()
                        });
                        break;
                    };
                    let (added, _, counted) = references.entry(member.clone()).or_default();
                    *added += 1;
                    if stored {
                        *counted += 1;
                    }
                    Some(containers::reference_text(&member))
                }
            };
            match text {
                Some(text) => new.set_child(om_scripts::text_node(&key, &text)),
                None => {
                    new.remove(&[&key]);
                }
            }
        }
        if journal.refused.is_none() {
            let deltas = references
                .iter()
                .map(|(member, &(added, removed, counted))| {
                    let surviving =
                        naming.get(member).copied().unwrap_or_default() - i64::from(removed);
                    let release =
                        self.guarded_container_release(table, member, counted, surviving, removed);
                    (member.clone(), added - counted - release)
                })
                .collect();
            self.adjust_container_counts(&mut journal, table, deltas);
        }
        if let Some(reason) = journal.refused.take() {
            warn!(
                "{}: table {name:?}: record {record:02x?} keeps its container calculation \
                 values: {reason}",
                self.name
            );
            self.roll_back(journal);
            return;
        }
        if let Some(outer) = outer {
            outer.absorb(journal);
            outer.save_record(table, &old);
        }
        if let Some(node) = self.root.at_mut(&path) {
            for (member, (added, removed, _)) in references {
                *naming.entry(member).or_default() += i64::from(added - removed);
            }
            *node = new;
            self.dirty = true;
        }
    }

    /// The member of `table`'s library a recomputed container value names
    /// when it is one of that library's, with `false`; else the member
    /// [`Self::store_container_with`] stores its data in, counted, with
    /// `true`: another table's member's tree as stored there, or the tree
    /// the field stores for data a calculation made
    /// (`om_data::containers::tree`). `None` without data to store, or
    /// when the library refused it (in `journal`).
    fn recomputed_member(
        &mut self,
        journal: &mut Journal,
        table: &[u8],
        value: &om_calc::Container,
        unique_id: &mut dyn FnMut() -> [u8; 16],
    ) -> Option<(Vec<u8>, bool)> {
        let mut data = None;
        if let Some(r) = &value.library {
            let library = [uint_segment(r.table), vec![LIBRARY]];
            let segments: Vec<&[u8]> = library.iter().map(Vec::as_slice).collect();
            let member = self
                .library_member(&segments, &r.member)
                .filter(|m| MemberInfo::read(m).is_some_and(|i| i.md5 == r.md5));
            if let Some(member) = member {
                if library[0] == table {
                    return Some((r.member.clone(), false));
                }
                data = library::member_tree(member);
            }
        }
        let data = match data {
            Some(data) => data,
            None if value.streams.is_empty() => return None,
            None => {
                let (keys, md5) = containers::tree(value)?;
                BinaryData::from_tree(keys, md5)
            }
        };
        let member = self.store_container_with(journal, table, data, unique_id);
        (!member.is_empty()).then_some((member, true))
    }

    /// The cells holding repetitions 2 to 255 of `field`, in a record of
    /// `table` or among its global values: `field` followed by the byte n
    /// (calc-values.md, "Repetitions in records"), but none that is
    /// another field's own key, in the catalog or in `others` (fields the
    /// same work names, which may be gone from the catalog).
    pub(crate) fn repetition_cells(
        &self,
        table: &[u8],
        field: &[u8],
        others: &BTreeSet<Vec<u8>>,
    ) -> BTreeSet<Vec<u8>> {
        self.derived.repetition_cells(table, field, others)
    }

    /// The cells holding `field`'s repetitions `from` and above: the field's
    /// own cell (repetition 1) from 1, and those of [`Self::repetition_cells`]
    /// whose repetition byte is at least `from`.
    fn cells_from(
        &self,
        table: &[u8],
        field: &[u8],
        from: u16,
        others: &BTreeSet<Vec<u8>>,
    ) -> BTreeSet<Vec<u8>> {
        let mut cells = self.repetition_cells(table, field, others);
        cells.retain(|cell| cell.last().is_some_and(|n| u16::from(*n) >= from));
        if from <= 1 {
            cells.insert(field.to_vec());
        }
        cells
    }

    /// Release names removed by deferred work after protecting the names
    /// that still appear in records and globals.
    fn release_containers(
        &mut self,
        mut journal: Option<&mut Journal>,
        table: &[u8],
        removed: BTreeMap<Vec<u8>, i32>,
        naming: &BTreeMap<Vec<u8>, i64>,
    ) {
        let path: [&[u8]; 2] = [table, &[LIBRARY]];
        for (member, removed) in removed {
            let surviving = naming.get(&member).copied().unwrap_or_default();
            let release = self.guarded_container_release(table, &member, 0, surviving, removed);
            if release == 0 {
                continue;
            }
            if let Some(reason) = self.library_refusal(&path, None, Some((&member, -release))) {
                warn!(
                    "{}: container data {member:02x?} of table {table:02x?} not released: {reason}",
                    self.name
                );
                continue;
            }
            if let Some(journal) = journal.as_deref_mut() {
                let changed = self
                    .node(&path)
                    .and_then(|l| library::adjust_keys(l, &member, -release))
                    .unwrap_or_default();
                for keys in changed {
                    let mut at = vec![table.to_vec(), vec![LIBRARY]];
                    at.extend(keys);
                    journal.save_node(&self.root, at);
                }
            }
            if let Some(library) = self.root.at_mut(&path) {
                library::adjust(library, &member, -release);
            }
        }
    }

    fn collect_removed_references(values: Vec<OwnedNode>, removed: &mut BTreeMap<Vec<u8>, i32>) {
        for member in values.iter().filter_map(member_key) {
            let count = removed.entry(member).or_default();
            *count = count.saturating_add(1);
        }
    }

    /// Delete global field `field`'s repetitions `from` and above (every
    /// one from 1) from `table`'s global values `[T, 08, F]` (calc-values.md,
    /// "Global values"), releasing a container's library members.
    fn drop_global_value(
        &mut self,
        journal: Option<&mut Journal>,
        table: &[u8],
        field: &[u8],
        (from, container): (u16, bool),
        others: &BTreeSet<Vec<u8>>,
        removed: &mut BTreeMap<Vec<u8>, i32>,
    ) {
        let cells = self.cells_from(table, field, from, others);
        let globals = [table, &[keys::GLOBALS]];
        let Some(values) = self.root.at(&globals) else {
            return;
        };
        if !values.children.iter().any(|c| cells.contains(&c.segment)) {
            return;
        }
        if let Some(journal) = journal {
            journal.save_node(&self.root, vec![table.to_vec(), vec![keys::GLOBALS]]);
        }
        let Some(globals) = self.root.at_mut(&globals) else {
            return;
        };
        let (gone, kept): (Vec<OwnedNode>, Vec<OwnedNode>) = std::mem::take(&mut globals.children)
            .into_iter()
            .partition(|c| cells.contains(&c.segment));
        globals.children = kept;
        if gone.is_empty() {
            return;
        }
        if container {
            Self::collect_removed_references(gone, removed);
        }
        self.dirty = true;
    }

    /// Rewrite `field`'s stored values as dates (or, with `timestamp`, as
    /// timestamps) after its type changed to one (schema-path.md 7.2.1;
    /// [`om_calc::datetime::type_change_text`]): every record and every
    /// repetition it stores, read in the file's locale with the host's
    /// current year, a value written only when its length changed. A
    /// record whose values change counts one more modification, as a
    /// deleted field's do; one whose count is the largest keeps its values.
    /// A styled (nested) value stays as stored. Whether any changed.
    fn convert_field_values(
        &mut self,
        mut journal: Option<&mut Journal>,
        table: &[u8],
        field: &[u8],
        timestamp: bool,
        others: &BTreeSet<Vec<u8>>,
        environment: &mut dyn FnMut() -> RecordEnvironment,
    ) -> bool {
        let locale = self.locale();
        let clock = environment().date;
        let Ok(Some(today)) = om_calc::Date::parse(
            &clock,
            &Locale::en_us(),
            om_calc::Date::from_ymd(2000, 1, 1).expect("a date"),
        ) else {
            warn!(
                "{}: table {table:02x?} field {field:02x?}: stored values not converted: the \
                 host's date {clock:?} does not read",
                self.name
            );
            return false;
        };
        let cells = self.repetition_cells(table, field, others);
        let is_value = |c: &OwnedNode| c.segment == field || cells.contains(&c.segment);
        let (mut converted, mut kept) = (0usize, 0usize);
        if let Some(records) = self.root.at_mut(&[table, &[keys::RECORDS]]) {
            for record in &mut records.children {
                let rewrites: Vec<(usize, String)> = record
                    .children
                    .iter()
                    .enumerate()
                    .filter(|(_, cell)| is_value(cell))
                    .filter_map(|(at, cell)| {
                        let stored = cell_text(cell)?;
                        let new = om_calc::datetime::type_change_text(
                            &stored, timestamp, &locale, today,
                        )?;
                        Some((at, new))
                    })
                    .collect();
                if rewrites.is_empty() {
                    continue;
                }
                if let Some(journal) = journal.as_deref_mut() {
                    journal.save_record(table, record);
                }
                if modcount::bump(record, 1).is_none() {
                    kept += 1;
                    continue;
                }
                for (at, new) in rewrites {
                    let segment = record.children[at].segment.clone();
                    record.children[at] = OwnedNode::leaf(segment, om_scsu::encode_scsu(&new));
                }
                converted += 1;
            }
        }
        if kept > 0 {
            warn!(
                "{}: {kept} record(s) of table {table:02x?} keep field {field:02x?}'s values as \
                 stored: their modification count is the largest a serial key holds",
                self.name
            );
        }
        if converted > 0 {
            note(
                &self.name,
                journal,
                format!(
                    "{converted} record(s) of table {table:02x?}: field {field:02x?}'s values \
                     rewritten as {}",
                    if timestamp { "timestamps" } else { "dates" }
                ),
            );
            self.dirty = true;
        }
        converted > 0
    }

    /// Remove a field's repetitions `from` and above (every one from 1)
    /// from every record of `table`, counting a modification on each record
    /// that had one (schema-path.md 7.3), and release container data the
    /// values named. A record whose modification count is the largest a
    /// serial key holds keeps the values and their container references:
    /// removed without a new count, a client's copy would not show as stale
    /// (#361).
    fn drop_field_values(
        &mut self,
        mut journal: Option<&mut Journal>,
        table: &[u8],
        field: &[u8],
        (from, container): (u16, bool),
        others: &BTreeSet<Vec<u8>>,
        removed: &mut BTreeMap<Vec<u8>, i32>,
    ) {
        let cells = self.cells_from(table, field, from, others);
        let is_value = |c: &OwnedNode| cells.contains(&c.segment);
        let mut kept = 0usize;
        if let Some(records) = self.root.at_mut(&[table, &[keys::RECORDS]]) {
            for record in &mut records.children {
                if !record.children.iter().any(is_value) {
                    continue;
                }
                if let Some(journal) = journal.as_deref_mut() {
                    journal.save_record(table, record);
                }
                if modcount::bump(record, 1).is_none() {
                    kept += 1;
                    continue;
                }
                let (gone, others): (Vec<OwnedNode>, Vec<OwnedNode>) =
                    std::mem::take(&mut record.children)
                        .into_iter()
                        .partition(is_value);
                record.children = others;
                if container {
                    Self::collect_removed_references(gone, removed);
                }
            }
        }
        if kept > 0 {
            warn!(
                "{}: {kept} record(s) of table {table:02x?} keep the deleted values of field \
                 {field:02x?}: their modification count is the largest a serial key holds",
                self.name
            );
        }
        self.dirty = true;
    }

    /// Build `field`'s value and word indexes from every record of `table`
    /// (query-program.md 7.2, 7.3), replacing any it had. With neither, the
    /// field is left without an index.
    fn build_index(
        &mut self,
        mut journal: Option<&mut Journal>,
        table: &[u8],
        field: &[u8],
        data_type: DataType,
        (value, word): (bool, bool),
    ) {
        let word = word && data_type == DataType::Text;
        match journal.as_deref_mut() {
            Some(journal) => {
                let index = vec![table.to_vec(), vec![keys::INDEXES], field.to_vec()];
                journal.take(&mut self.root, index.clone());
                let mut kind = index;
                kind.push(vec![keys::VALUE_INDEX]);
                journal.save_parents(&self.root, &kind);
            }
            None => {
                self.root.remove(&[table, &[keys::INDEXES], field]);
            }
        }
        self.dirty = true;
        // An index language whose keys this server does not build leaves
        // the field without an index rather than with English keys (#159):
        // the one it had no longer matches what changed.
        let Some(form) = self.written_form(table, field, data_type) else {
            warn!(
                "{}: table {table:02x?} field {field:02x?} left without an index: its records \
                 hold text, and OpenMaker does not build the keys of its index language (#1279)",
                self.name
            );
            return;
        };
        if value {
            let index = self.index_keys(table, field, data_type, keys::VALUE_INDEX, form);
            self.put_index(table, field, keys::VALUE_INDEX, index);
        }
        if word {
            let index = self.index_keys(table, field, data_type, keys::WORD_INDEX, form);
            self.put_index(table, field, keys::WORD_INDEX, index);
        }
        note(
            &self.name,
            journal,
            format!(
                "built the indexes of table {table:02x?} field {field:02x?} (value {value}, \
                 word {word})"
            ),
        );
        self.dirty = true;
    }

    /// The keys of `field`'s value or word index over every record of
    /// `table`, each with the records holding it (query-program.md 7.2,
    /// 7.3).
    fn index_keys(
        &self,
        table: &[u8],
        field: &[u8],
        data_type: DataType,
        kind: u8,
        form: TextKeys,
    ) -> BTreeMap<Vec<u8>, RidList> {
        self.index_entries(table, field, data_type, kind, form, false)
            .unwrap_or_default()
    }

    /// [`Store::index_keys`], or `None` when a record holds a value in the
    /// field that the current rules give no value-index key, which another
    /// rule (FileMaker's) may have given one.
    fn accounted_index(
        &self,
        table: &[u8],
        field: &[u8],
        data_type: DataType,
        kind: u8,
        form: TextKeys,
    ) -> Option<BTreeMap<Vec<u8>, RidList>> {
        self.index_entries(table, field, data_type, kind, form, true)
    }

    fn index_entries(
        &self,
        table: &[u8],
        field: &[u8],
        data_type: DataType,
        kind: u8,
        form: TextKeys,
        accounted: bool,
    ) -> Option<BTreeMap<Vec<u8>, RidList>> {
        derived::index_entries(
            &self.tree_view(),
            &self.derived,
            table,
            field,
            data_type,
            kind,
            form,
            accounted,
        )
        .expect("the file engine's tree never fails a read")
    }

    /// The field `field` of `table` when its index `kind` can be built on
    /// demand ([`Derived::indexable`]).
    fn indexable(&self, table: &[u8], field: &[u8], kind: u8) -> Option<DataType> {
        self.derived.indexable(table, field, kind)
    }

    /// Note the device and inode of the file on disk the store was read
    /// from ([`anchored::file_identity`](crate::anchored::file_identity)).
    pub fn set_file(&mut self, file: Option<FileId>) {
        self.file = file;
    }

    /// The device and inode of the file on disk the store was read from or
    /// last saved as, where it has them.
    pub fn file(&self) -> Option<FileId> {
        self.file
    }

    /// Take `built` back from the indexes due to be stored
    /// ([`Store::store_indexes`]). A query refused for an index
    /// this server cannot key keeps none of the others it built (#572), so
    /// the client that asks next which indexes are missing sees the file as
    /// the query found it, whichever query stores the due indexes first. A
    /// later query builds them again.
    pub fn forget_indexes_on_demand(&self, built: &[IndexId]) {
        self.waiting.forget_indexes(built);
    }

    /// Whether a query read indexes that were built on demand and are not
    /// yet stored.
    pub fn has_indexes_on_demand(&self) -> bool {
        self.waiting.has_indexes()
    }

    /// Take the indexes queries built on demand and nobody stored yet, for
    /// [`Store::store_indexes`]: they are no longer waiting.
    pub fn take_indexes_on_demand(&mut self) -> Vec<IndexId> {
        self.waiting.take_indexes()
    }

    /// Store `indexes`, which queries built on demand
    /// ([`Store::take_indexes_on_demand`]), so that record writes keep them
    /// current from now on (query-program.md 7.1). Each is built again
    /// here, under the write lock, from the records as they are now.
    #[cfg(test)]
    pub fn store_indexes(&mut self, indexes: Vec<IndexId>) {
        let mut journal = self.begin_commit();
        self.store_indexes_in(&mut journal, indexes);
    }

    /// Put `indexes`, taken by [`Store::take_indexes_on_demand`] and not
    /// stored, back among those waiting: the write that took them was not
    /// stored, so the next one stores them.
    pub fn put_back_indexes(&mut self, indexes: Vec<IndexId>) {
        self.waiting.put_back_indexes(indexes);
    }

    /// [`Store::store_indexes`] within `journal`, which keeps what each
    /// index replaces (nothing, as a rule) and the parents it creates.
    pub fn store_indexes_in(&mut self, journal: &mut Journal, indexes: Vec<IndexId>) {
        for (table, field, kind) in indexes {
            let index = [table.as_slice(), &[keys::INDEXES], &field];
            if self
                .node(&[index[0], index[1], index[2], &[counts_key(kind)]])
                .is_some()
            {
                continue;
            }
            let Some(data_type) = self.indexable(&table, &field, kind) else {
                continue;
            };
            // A query builds no index in a language whose keys this server
            // does not build while a record holds text there (#572); one
            // built while none did, a record of which has gained text since,
            // is not stored (#159), and the next query is refused.
            let Some(form) = self.written_form(&table, &field, data_type) else {
                continue;
            };
            let index = self.index_keys(&table, &field, data_type, kind, form);
            for key in [kind, counts_key(kind)] {
                let path = vec![table.clone(), vec![keys::INDEXES], field.clone(), vec![key]];
                journal.save_path(&self.root, path);
            }
            if self.put_index(&table, &field, kind, index) {
                journal.note(format!(
                    "built index {kind} of table {table:02x?} field {field:02x?} on demand"
                ));
                self.dirty = true;
            }
        }
    }

    /// Write one index and its counts; an index without keys is its counts
    /// alone, as [`Store::move_record`] leaves an emptied one. An index the
    /// file cannot hold is not stored (its keys are cut to fit, so this
    /// does not happen; logged if it does): the field stays without it, and
    /// a find that reads it is answered from one built for that find.
    /// Returns whether it was stored.
    fn put_index(
        &mut self,
        table: &[u8],
        field: &[u8],
        index: u8,
        keys: BTreeMap<Vec<u8>, RidList>,
    ) -> bool {
        let counts = counts_key(index);
        let entries: usize = keys.values().map(RidList::len).sum();
        let distinct = keys.len();
        let parents: [&[u8]; 3] = [table, &[keys::INDEXES], field];
        let refused = put_refusals(
            &self.root,
            &parents,
            &OwnedNode::leaf(vec![counts], vec![0; 16]),
            3,
        );
        if !refused.is_empty() {
            error!(
                "{}: index {index} of table {table:02x?} field {field:02x?} cannot be \
                 stored: {}",
                self.name,
                describe(&refused)
            );
            return false;
        }
        if !keys.is_empty() {
            let nodes = keys
                .into_iter()
                .map(|(key, ids)| ids.to_node(key))
                .collect();
            let node = OwnedNode::list(vec![index], nodes);
            let refused = put_refusals(&self.root, &parents, &node, 3);
            if !refused.is_empty() {
                error!(
                    "{}: index {index} of table {table:02x?} field {field:02x?} cannot be \
                     stored: {}",
                    self.name,
                    describe(&refused)
                );
                return false;
            }
            self.root
                .put(&[table, &[keys::INDEXES], field, &[index]], node);
        }
        let mut value = (entries as u64).to_be_bytes().to_vec();
        value.extend((distinct as u64).to_be_bytes());
        self.root.put(
            &[table, &[keys::INDEXES], field, &[counts]],
            OwnedNode::leaf(Vec::new(), value),
        );
        true
    }

    /// A store holding only `records` of `table` (and the table's field
    /// definitions), for a query that must see records as a script's
    /// transaction left them: its indexes are built on demand from these
    /// records alone.
    pub fn scratch(&self, table: &[u8], records: &[(u64, &OwnedNode)]) -> Store {
        let mut root = OwnedNode::default();
        if let Some(fields) = self.node(&[table, &[keys::FIELDS]]) {
            root.put(&[table, &[keys::FIELDS]], fields.clone());
        }
        let mut ids = RidList::new();
        for (id, record) in records {
            let segment = serial::encode(*id);
            root.put(&[table, &[keys::RECORDS], &segment], (*record).clone());
            ids.insert(*id);
        }
        root.put(&[table, &[keys::RECORD_IDS]], ids.to_node(Vec::new()));
        Store {
            root,
            version: 0,
            derived: self.derived.clone(),
            name: self.name.clone(),
            basis: HashMap::new(),
            superblock: Vec::new(),
            dirty: false,
            saves: 0,
            #[cfg(debug_assertions)]
            fail_commit_saves: false,
            #[cfg(debug_assertions)]
            save_fault: None,
            unknown: None,
            generation: 0,
            accounts_at: 0,
            waiting: Waiting::default(),
            verified: (0, BTreeMap::new()),
            lock: None,
            file: None,
            read_only: None,
        }
    }

    /// Whether `record` exists in `table`.
    pub fn has_record(&self, table: &[u8], record: u64) -> bool {
        self.node(&[table, &[keys::RECORDS], &serial::encode(record)])
            .is_some()
    }

    /// The modification count of the node at `path` as op60 returns it: its
    /// `fc` child, `01 00` when it has none, empty when the node does not
    /// exist. A record (`[T, 05, R]`, write-path.md 4.2, 5.4), a layout or a
    /// catalog index alike (schema-path.md 4.1). An `fc` that is not a count
    /// ([`modcount::is_count`]: a file written before commits were checked,
    /// or by another program) is `01 00` too, the 0 the server's scripts
    /// read for it; so the value is at most 10 bytes and op60's reply can
    /// always carry it.
    pub fn mod_count(&self, path: &[Vec<u8>]) -> Vec<u8> {
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let Some(node) = self.node(&segments) else {
            return Vec::new();
        };
        match node.child(&[MOD_COUNT]) {
            Some(count) if modcount::is_count(count) => count.value.clone().unwrap_or_default(),
            Some(_) => {
                warn!(
                    "{}: {path:02x?} holds a modification count that is not one; reported as none",
                    self.name
                );
                modcount::key_string(0)
            }
            None => modcount::key_string(0),
        }
    }

    /// Write `record` (a new or modified record node, as uploaded) as record
    /// `id` of `table`, or delete the record when `record` is `None`, keeping
    /// `[T, 0d]` and the field indexes current (write-path.md 5.2-5.5).
    pub fn write_record(&mut self, table: &[u8], id: u64, record: Option<OwnedNode>) {
        let segment = serial::encode(id);
        let old = self.node(&[table, &[keys::RECORDS], &segment]).cloned();
        let (existed, exists) = (old.is_some(), record.is_some());
        let mut recount = BTreeSet::new();
        if let Err(reason) =
            self.put_record(table, id, old.as_ref(), record, None, &mut recount, true)
        {
            error!(
                "{}: record {id} of table {table:02x?} not written: {reason}",
                self.name
            );
            return;
        }
        for (table, field, index, counts) in recount {
            self.recount_index(&table, &field, index, counts);
        }
        match (existed, exists) {
            (false, true) => self.update_record_ids(table, |ids| {
                ids.insert(id);
            }),
            (true, false) => {
                self.update_record_ids(table, |ids| *ids = ids.and_not(&[id].into_iter().collect()))
            }
            _ => {}
        }
    }

    /// Store `record` (or remove it) as record `id` of `table`, `old` as it
    /// was, moving it between the keys of the table's indexes; the indexes
    /// whose counts are then due go into `recount`. Every index entry a
    /// move touches is kept in `journal` first, so a failed save puts each
    /// back as it was, whatever rule wrote it. With `sweep`, a record that
    /// existed leaves the entries of an index that disagrees with the
    /// current rules ([`Store::index_agrees`]) other than the ones its new
    /// value gives: when `journal`'s commit finishes, or at once without one
    /// ([`Store::sweep_index`]). A roll-back moves without sweeping. The
    /// record list is the caller's. A write the tree cannot take as it is
    /// ([`Store::record_refusal`]) changes nothing and returns why.
    #[allow(clippy::too_many_arguments)]
    fn put_record(
        &mut self,
        table: &[u8],
        id: u64,
        old: Option<&OwnedNode>,
        record: Option<OwnedNode>,
        mut journal: Option<&mut Journal>,
        recount: &mut BTreeSet<Recount>,
        sweep: bool,
    ) -> Result<(), String> {
        let segment = serial::encode(id);
        let path: [&[u8]; 3] = [table, &[keys::RECORDS], &segment];
        let plan = self.reindex_plan(table, id, old, record.as_ref(), recount)?;
        if let Some(reason) = self.record_refusal(table, &segment, record.as_ref(), &plan) {
            return Err(reason);
        }
        self.dirty = true;
        let mut sweeps = Vec::new();
        for (field, index, before, after) in plan {
            if let Some(journal) = journal.as_deref_mut().filter(|_| before != after) {
                let path: Vec<Vec<u8>> = vec![
                    table.to_vec(),
                    vec![keys::INDEXES],
                    field.clone(),
                    vec![index],
                ];
                // A move that may empty the index removes its node, and one
                // into an index without keys creates it: the node is kept as
                // it was (or its absence), so a failed save leaves no list
                // behind and finds the index to move the record back into.
                let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
                if self
                    .node(&segments)
                    .is_none_or(|node| node.children.len() <= before.len())
                {
                    journal.save_node(&self.root, path.clone());
                }
                for key in before.union(&after) {
                    let mut entry = path.clone();
                    entry.push(key.clone());
                    journal.save_node(&self.root, entry);
                }
            }
            if sweep && old.is_some() && !self.index_agrees(table, &field, index) {
                sweeps.push((field.clone(), index, after.clone()));
            }
            if self.move_record(table, &field, index, id, &before, &after) {
                recount.insert((table.to_vec(), field, index, counts_key(index)));
            }
        }
        for (field, index, after) in sweeps {
            let records = BTreeMap::from([(id, after)]);
            match journal.as_deref_mut() {
                Some(journal) => journal
                    .sweep
                    .entry((table.to_vec(), field, index))
                    .or_default()
                    .extend(records),
                None => self.sweep_index(None, recount, (table, &field, index), &records),
            }
        }
        match record {
            Some(record) => self.root.put(
                &path,
                OwnedNode {
                    push: true,
                    ..record
                },
            ),
            None => {
                self.root.remove(&path);
            }
        }
        Ok(())
    }

    /// Why a write of `record` (or its deletion) as `[T, 05, segment]`, with
    /// the index changes `plan`, would change something it does not name:
    /// a node it writes through or into that holds a value
    /// (`om_hbam::writer::value_on_path`; the table, the record list and
    /// records, an index the write changes, an index entry it adds the
    /// record to or takes it from), or a record the file cannot hold there
    /// (`put_refusals`). `None` when the write is safe.
    fn record_refusal(
        &self,
        table: &[u8],
        segment: &[u8],
        record: Option<&OwnedNode>,
        plan: &[Reindex],
    ) -> Option<String> {
        let parents: [&[u8]; 2] = [table, &[keys::RECORDS]];
        if let Some(record) = record {
            // As `put_record` puts it: named by its id, a list.
            let renamed;
            let placed = if record.segment == segment && record.push {
                record
            } else {
                renamed = OwnedNode {
                    segment: segment.to_vec(),
                    push: true,
                    ..record.clone()
                };
                &renamed
            };
            let refused = put_refusals(&self.root, &parents, placed, 3);
            if !refused.is_empty() {
                return Some(describe(&refused));
            }
        } else if let Some(path) = value_on_path(&self.root, &parents) {
            return Some(describe_value(&path));
        }
        for (field, index, before, after) in plan {
            let path: [&[u8]; 4] = [table, &[keys::INDEXES], field, &[*index]];
            if let Some(path) = value_on_path(&self.root, &path) {
                return Some(describe_value(&path));
            }
            // A move can replace a posting in the difference, and a sweep
            // can add a missing membership even to a key on both sides.
            for key in before.union(after) {
                let entry: [&[u8]; 5] = [path[0], path[1], path[2], path[3], key];
                let Some(posting) = self.node(&entry) else {
                    continue;
                };
                if holds_value(posting) {
                    return Some(describe_value(&entry.map(<[u8]>::to_vec)));
                }
                if plain_entry(posting).is_none() {
                    return Some(format!(
                        "index posting {entry:02x?} has a RID encoding this server cannot edit losslessly"
                    ));
                }
            }
        }
        None
    }

    fn update_record_ids(&mut self, table: &[u8], change: impl FnOnce(&mut RidList)) {
        let path: [&[u8]; 2] = [table, &[keys::RECORD_IDS]];
        let mut ids = self
            .node(&path)
            .and_then(|n| RidList::from_owned(n).ok())
            .unwrap_or_default();
        change(&mut ids);
        self.root.put(&path, ids.to_node(Vec::new()));
    }

    /// The index changes a write of record `id` of `table` from `old` to
    /// `new` makes: for every indexed field of `table` whose entries or
    /// stored value differ, or every one when the record is created or
    /// deleted (write-path.md 5.5), the field, the index, and the record's
    /// entries before and after. An index the commit already changed
    /// (`pending`) counts as present though its last key went. A planned
    /// index whose keys do not move still lets [`Store::put_record`] sweep
    /// the record's entries another rule wrote. Refused, saying why, when
    /// it would change the text
    /// of a field indexed in a language whose keys this server does not
    /// build ([`Self::text_keys`]): a new or deleted record with text there,
    /// or another text. English keys there would not be FileMaker's, and
    /// finds would miss the record (#159).
    fn reindex_plan(
        &self,
        table: &[u8],
        id: u64,
        old: Option<&OwnedNode>,
        new: Option<&OwnedNode>,
        pending: &BTreeSet<Recount>,
    ) -> Result<Vec<Reindex>, String> {
        let mut plan = Vec::new();
        let Some(definition) = be_uint(table).and_then(|t| self.derived.catalog().table(t)) else {
            return Ok(plan);
        };
        // Per field: whether it has a value index and a word index.
        let mut present: BTreeMap<Vec<u8>, (bool, bool)> = BTreeMap::new();
        for index in self
            .node(&[table, &[keys::INDEXES]])
            .map_or(&[][..], |n| &n.children)
        {
            let has = |a: u8, b: u8| index.child(&[a]).is_some() || index.child(&[b]).is_some();
            let kinds = present.entry(index.segment.clone()).or_default();
            kinds.0 |= has(keys::VALUE_INDEX, keys::VALUE_COUNTS);
            kinds.1 |= has(keys::WORD_INDEX, keys::WORD_COUNTS);
        }
        for (_, field, index, _) in pending.iter().filter(|(t, ..)| t.as_slice() == table) {
            let kinds = present.entry(field.clone()).or_default();
            kinds.0 |= *index == keys::VALUE_INDEX;
            kinds.1 |= *index == keys::WORD_INDEX;
        }
        let mut work = Vec::new();
        for (segment, (value, word)) in present {
            let Some(field) = be_uint(&segment).and_then(|f| definition.field(f)) else {
                continue;
            };
            let word = word && field.data_type == DataType::Text;
            work.push((segment, field.data_type, value, word));
        }
        let changes = old.is_some() != new.is_some();
        for (field, data_type, value, word) in work {
            let repetitions = self.repetitions(table, &field);
            let texts = |record: Option<&OwnedNode>| {
                record
                    .map(|r| field_texts(r, &field, repetitions))
                    .unwrap_or_default()
            };
            let (old_texts, new_texts) = (texts(old), texts(new));
            let form = match self.text_keys(table, &field) {
                Some(form) => form,
                None if data_type != DataType::Text => ENGLISH,
                None => {
                    let held = |texts: &[String]| texts.iter().any(|t| !t.is_empty());
                    if old_texts != new_texts && (held(&old_texts) || held(&new_texts)) {
                        return Err(format!(
                            "record {id} would change the text of field {field:02x?}, indexed \
                             in a language whose keys OpenMaker does not build (#1279)"
                        ));
                    }
                    ENGLISH
                }
            };
            if value {
                let keys_of = |record: Option<&OwnedNode>, texts: &[String]| {
                    if record.is_none() {
                        return BTreeSet::new();
                    }
                    record_value_entries(data_type, texts, self.reading(), form)
                };
                let (before, after) = (keys_of(old, &old_texts), keys_of(new, &new_texts));
                if new.is_some() && after.is_empty() && new_texts.iter().any(|t| !t.is_empty()) {
                    warn!("no index key for {data_type:?} value {new_texts:?}; record {id} is left out of the index");
                }
                if before != after || old_texts != new_texts || changes {
                    plan.push((field.clone(), keys::VALUE_INDEX, before, after));
                }
            }
            if word {
                let (before, after) = (
                    word_entries(&old_texts, form),
                    word_entries(&new_texts, form),
                );
                if before != after || old_texts != new_texts || changes {
                    plan.push((field.clone(), keys::WORD_INDEX, before, after));
                }
            }
        }
        Ok(plan)
    }

    /// Whether index `kind` of `field` holds the keys the current rules give
    /// every stored record of `table` (#448), checked once per schema
    /// generation against an index built from the records
    /// ([`Store::index_keys`]), entry by entry on the compact lists. An
    /// index another rule wrote (an earlier OpenMaker build's, or
    /// FileMaker's where OpenMaker's rules differ or do not key a value)
    /// disagrees; so does one with an entry that is not a plain record list
    /// ([`plain_entry`]). A disagreeing index is never rebuilt, so the
    /// memberships of records no write touches stay as FileMaker wrote
    /// them; instead a record changed in it leaves its other entries
    /// ([`Store::sweep_index`]). A text field in a language whose keys this
    /// server does not build counts as agreeing: no write changes its text
    /// ([`Store::reindex_plan`] refuses that).
    fn index_agrees(&mut self, table: &[u8], field: &[u8], kind: u8) -> bool {
        if self.verified.0 != self.generation {
            self.verified = (self.generation, BTreeMap::new());
        }
        let id: IndexId = (table.to_vec(), field.to_vec(), kind);
        if let Some(agrees) = self.verified.1.get(&id) {
            return *agrees;
        }
        let agrees = self.check_index(table, field, kind);
        self.verified.1.insert(id, agrees);
        agrees
    }

    fn check_index(&self, table: &[u8], field: &[u8], kind: u8) -> bool {
        let Some(data_type) = be_uint(table)
            .and_then(|t| self.derived.catalog().table(t))
            .and_then(|t| be_uint(field).and_then(|f| t.field(f)))
            .map(|f| f.data_type)
        else {
            return true;
        };
        let form = match self.text_keys(table, field) {
            Some(form) => form,
            None if data_type != DataType::Text => ENGLISH,
            None => return true,
        };
        let Some(built) = self.accounted_index(table, field, data_type, kind, form) else {
            return false;
        };
        let path: [&[u8]; 4] = [table, &[keys::INDEXES], field, &[kind]];
        match self.node(&path) {
            None => built.is_empty(),
            Some(node) => {
                node.value.is_none()
                    && node.children.len() == built.len()
                    && node.children.iter().all(|entry| {
                        plain_entry(entry)
                            .is_some_and(|ids| built.get(&entry.segment) == Some(&ids))
                    })
            }
        }
    }

    /// Make `records` (each with the keys its value gives) hold exactly those
    /// keys in `index` (table, field, kind), one that disagrees with the
    /// current rules: each leaves every other entry, as an entry another
    /// rule wrote for its old value (`1.5` read by an earlier build, a
    /// FileMaker word break, a number among letters) is not among the keys
    /// its move took it from; and each is put under every key of its value
    /// it is missing from, as its move added none when the computed keys
    /// did not change (`a_b` to `a b` in Unicode raw). Other records'
    /// memberships are not touched. Each entry changed goes into `journal`
    /// first. The index's counts go into `recount`. Entries outside the
    /// computed keys that fail [`plain_entry`] stay as stored.
    /// [`Store::record_refusal`] checks every computed key before any move.
    fn sweep_index(
        &mut self,
        journal: Option<&mut Journal>,
        recount: &mut BTreeSet<Recount>,
        (table, field, kind): (&[u8], &[u8], u8),
        records: &BTreeMap<u64, BTreeSet<Vec<u8>>>,
    ) {
        let path: [&[u8]; 4] = [table, &[keys::INDEXES], field, &[kind]];
        let swept: RidList = records.keys().copied().collect();
        // Per key, the records its entry must hold.
        let mut wanted: BTreeMap<&[u8], RidList> = BTreeMap::new();
        for (id, keys) in records {
            for key in keys {
                wanted.entry(key).or_default().insert(*id);
            }
        }
        // The entries to write (`None`: removed), by key.
        let mut changed: BTreeMap<Vec<u8>, Option<OwnedNode>> = BTreeMap::new();
        for entry in self.node(&path).map_or(&[][..], |n| &n.children) {
            let want = wanted.remove(entry.segment.as_slice());
            let Some(list) = plain_entry(entry) else {
                continue;
            };
            let leaving = list
                .and(&swept)
                .and_not(want.as_ref().unwrap_or(&RidList::new()));
            let joining = want.map_or_else(RidList::new, |w| w.and_not(&list));
            if leaving.is_empty() && joining.is_empty() {
                continue;
            }
            let kept = list.and_not(&leaving).or(&joining);
            let replacement = (!kept.is_empty()).then(|| kept.to_node(entry.segment.clone()));
            changed.insert(entry.segment.clone(), replacement);
        }
        // Only missing keys remain in `wanted`.
        for (key, ids) in wanted {
            changed.insert(key.to_vec(), Some(ids.to_node(key.to_vec())));
        }
        if changed.is_empty() {
            return;
        }
        if let Some(journal) = journal {
            let base: Vec<Vec<u8>> = path.iter().map(|s| s.to_vec()).collect();
            if self.node(&path).is_none() {
                journal.save_node(&self.root, base.clone());
            }
            for key in changed.keys() {
                let mut entry = base.clone();
                entry.push(key.clone());
                journal.save_node(&self.root, entry);
            }
        }
        // Move untouched postings into the merged list after journaling
        // the replacements, so even a large unchanged posting stays intact.
        let old_len = self.node(&path).map_or(0, |n| n.children.len());
        let mut entries = Vec::with_capacity(old_len + changed.len());
        let old = self
            .root
            .at_mut(&path)
            .map(|n| std::mem::take(&mut n.children))
            .unwrap_or_default();
        let mut changes = changed.into_iter().peekable();
        for entry in old {
            while let Some((_, replacement)) = changes.next_if(|(key, _)| *key < entry.segment) {
                entries.extend(replacement);
            }
            match changes.next_if(|(key, _)| *key == entry.segment) {
                Some((_, replacement)) => entries.extend(replacement),
                None => entries.push(entry),
            }
        }
        entries.extend(changes.filter_map(|(_, replacement)| replacement));
        if entries.is_empty() {
            self.root.remove(&path);
        } else {
            self.root.put(&path, OwnedNode::list(vec![kind], entries));
        }
        recount.insert((table.to_vec(), field.to_vec(), kind, counts_key(kind)));
    }

    /// Remove `id` from the index keys only in `before` and add it to those
    /// only in `after`. Returns whether the index changed (its counts are
    /// then due, [`Store::recount_index`]).
    fn move_record(
        &mut self,
        table: &[u8],
        field: &[u8],
        index: u8,
        id: u64,
        before: &BTreeSet<Vec<u8>>,
        after: &BTreeSet<Vec<u8>>,
    ) -> bool {
        if before == after {
            return false;
        }
        let path: [&[u8]; 4] = [table, &[keys::INDEXES], field, &[index]];
        let node = self.root.ensure(&path);
        for key in before.difference(after) {
            let Some(entry) = node.child_mut(key) else {
                continue;
            };
            let mut records = RidList::from_owned(entry).unwrap_or_default();
            records = records.and_not(&[id].into_iter().collect());
            if records.is_empty() {
                node.remove(&[key]);
            } else {
                entry.children = records.entries();
            }
        }
        for key in after.difference(before) {
            let entry = node.ensure(&[key]);
            let mut records = RidList::from_owned(entry).unwrap_or_default();
            records.insert(id);
            entry.children = records.entries();
        }
        if node.children.is_empty() {
            self.root.remove(&path);
        }
        true
    }

    /// Count an index's entries and distinct keys into its counts leaf
    /// (write-path.md 5.5; FileMaker Server's own counts drift, so these
    /// are recomputed, once per commit).
    fn recount_index(&mut self, table: &[u8], field: &[u8], index: u8, counts: u8) {
        // A counts node of another shape than the 16-byte leaf this writes
        // is FileMaker's or unknown: it is left as it is, not replaced.
        if self
            .node(&[table, &[keys::INDEXES], field, &[counts]])
            .is_some_and(|node| {
                node.value.as_ref().is_none_or(|v| v.len() != 16) || !node.children.is_empty()
            })
        {
            warn!(
                "{}: the counts of index {index} of field {field:02x?} in table {table:02x?} \
                 have an unknown shape and are left as they are",
                self.name
            );
            return;
        }
        let (entries, distinct) = self
            .node(&[table, &[keys::INDEXES], field, &[index]])
            .map_or((0, 0), |node| {
                let entries: usize = node
                    .children
                    .iter()
                    .map(|k| RidList::from_owned(k).map_or(0, |r| r.len()))
                    .sum();
                (entries, node.children.len())
            });
        let mut value = (entries as u64).to_be_bytes().to_vec();
        value.extend((distinct as u64).to_be_bytes());
        self.root.put(
            &[table, &[keys::INDEXES], field, &[counts]],
            OwnedNode::leaf(Vec::new(), value),
        );
    }
}

impl om_query::Source for Store {
    fn catalog(&self) -> om_hbam::Result<Catalog> {
        Ok(self.derived.catalog().clone())
    }

    fn node(&self, path: &[&[u8]]) -> om_hbam::Result<Option<Cow<'_, OwnedNode>>> {
        Ok(Store::node(self, path).map(Cow::Borrowed))
    }

    /// Build a missing index from the stored records for a query that reads
    /// it ([`derived::index_on_demand`]); [`Store::store_indexes`] stores it
    /// afterwards.
    fn index_on_demand(
        &self,
        table: &[u8],
        field: &[u8],
        kind: u8,
    ) -> Result<Option<OwnedNode>, om_query::QueryError> {
        derived::index_on_demand(
            &self.tree_view(),
            &self.derived,
            &self.waiting,
            table,
            field,
            kind,
        )
    }

    /// Issue the catalog's next member id and modification count
    /// ([`Store::issue_member_key`]), as key strings.
    fn generate_member_key(&self, catalog: &[Vec<u8>]) -> Option<(Vec<u8>, Vec<u8>)> {
        let next = self.issue_member_key(catalog)?;
        Some((modcount::key_string(next.0), modcount::key_string(next.1)))
    }
}

/// A table's library for the calculations of a recompute
/// ([`Store::recalculate`]).
struct LibraryContainers {
    table: u64,
    library: std::sync::Arc<OwnedNode>,
}

impl LibraryContainers {
    fn member(&self, table: u64, member: &[u8]) -> Option<&OwnedNode> {
        (table == self.table)
            .then(|| library::member(&self.library, member))
            .flatten()
    }
}

impl om_data::rules::ContainerReader for LibraryContainers {
    fn value(&self, table: u64, text: &str) -> Option<om_calc::Container> {
        let member = containers::reference(text)?;
        let info = MemberInfo::read(self.member(table, &member)?)?;
        Some(info.value(table, &member))
    }

    fn data(&self, table: u64, member: &[u8]) -> Option<om_calc::eval::StoredContainer> {
        containers::read(self.member(table, member)?)
    }
}

/// The record list an index entry holds, when the entry is nothing but a
/// list of record-id runs written as [`RidList::entries`] writes them
/// (FileMaker Server's form, write-path.md 5.3): no value of its own, each
/// run a leaf. `None` if re-encoding would discard the stored shape.
fn plain_entry(entry: &OwnedNode) -> Option<RidList> {
    if entry.value.is_some() {
        return None;
    }
    let list = RidList::from_owned(entry).ok()?;
    let runs = list.entries();
    let same = runs.len() == entry.children.len()
        && runs.iter().zip(&entry.children).all(|(ours, stored)| {
            ours.segment == stored.segment
                && ours.value == stored.value
                && stored.children.is_empty()
        });
    same.then_some(list)
}

/// The library member a container field's value names (containers.md
/// 1.4, [`containers::reference`]).
fn member_key(value: &OwnedNode) -> Option<Vec<u8>> {
    containers::reference(&cell_text(value)?)
}

/// Advance a serial number's text by `by`: the rightmost run of digits is
/// incremented keeping its width ("INV0099" + 1 = "INV0100"); text without
/// digits gets the number appended (write-path.md 3.2, Med).
fn increment_serial(text: &str, by: i32) -> String {
    let bytes = text.as_bytes();
    let Some(end) = bytes.iter().rposition(u8::is_ascii_digit).map(|i| i + 1) else {
        return format!("{text}{by}");
    };
    let start = bytes[..end]
        .iter()
        .rposition(|b| !b.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let digits = &text[start..end];
    if let Ok(by) = u32::try_from(by) {
        // Digit by digit, however long the run: a value that stayed as it
        // was would be handed out again, and two records would share it.
        let mut next = digits.as_bytes().to_vec();
        let mut carry = by;
        for digit in next.iter_mut().rev() {
            if carry == 0 {
                break;
            }
            let sum = u64::from(*digit - b'0') + u64::from(carry);
            *digit = b'0' + (sum % 10) as u8;
            carry = (sum / 10) as u32;
        }
        let carried = if carry == 0 {
            String::new()
        } else {
            carry.to_string()
        };
        let next = std::str::from_utf8(&next).expect("ASCII digits");
        return format!("{}{carried}{next}{}", &text[..start], &text[end..]);
    }
    // A decrease (FileMaker Pro may send one; OpenMaker's client refuses
    // an increment below 1) stops at 0. A number too large to read stays
    // as it is.
    let Some(next) = digits
        .parse::<i128>()
        .ok()
        .and_then(|number| number.checked_add(i128::from(by)))
    else {
        return text.to_string();
    };
    let next = next.max(0);
    format!(
        "{}{:0width$}{}",
        &text[..start],
        next,
        &text[end..],
        width = digits.len()
    )
}

#[cfg(debug_assertions)]
impl Store {
    /// The last member id and modification count issued for `catalog`
    /// that wait to be written, and those the tree holds (the inputs of
    /// GenerateMemberKey, `om_query::Source::generate_member_key`).
    pub(crate) fn member_keys_for_tests(
        &self,
        catalog: &[Vec<u8>],
    ) -> (Option<(u64, u64)>, (u64, u64)) {
        let path: Vec<&[u8]> = catalog.iter().map(Vec::as_slice).collect();
        let stored = derived::stored_member_key(&self.tree_view(), &path)
            .expect("the file engine's tree never fails a read");
        (self.waiting.issued_for(catalog), stored)
    }
}

#[cfg(test)]
impl Store {
    /// The basis of stored calculations, by table ([`recalc`]): tests check
    /// that a change not stored leaves it as it was.
    pub(crate) fn basis_for_tests(&self) -> &HashMap<Vec<u8>, TableBasis> {
        &self.basis
    }

    /// What reads left waiting: the indexes built on demand and the member
    /// keys issued, each in order.
    pub(crate) fn waiting_for_tests(&self) -> (Vec<IndexId>, Vec<MemberKey>) {
        self.waiting.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The #137 fixture's `limited` set (FileMaker Pro 26.0.3, out of git):
    /// Contacts' edit limited by `First Name = "Allowed"`, stored at
    /// `[23, 2, 5, 04, 05, 80 02, 08, 06]`; its two records have First
    /// Name "Allowed" and "Denied".
    #[test]
    #[ignore = "requires the FileMaker-made fixture of #137 (OM_CUSTOM_SETS_FIXTURE, or re/work/137-custom-sets/Contacts-custom.fmp12; not in git)"]
    fn a_custom_sets_limiting_formula_evaluates_on_each_record() {
        let path = std::env::var_os("OM_CUSTOM_SETS_FIXTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../re/work/137-custom-sets/Contacts-custom.fmp12")
            });
        let file = HbamFile::open(&path).unwrap();
        let store = Store::load(&file, "Contacts-custom.fmp12").unwrap();
        let contacts = uint_segment(0x8002);
        let edit: &[&[u8]] = &[&[23], &[2], &[5], &[4], &[5], &contacts, &[8], &[6]];
        let code = store.node(edit).and_then(|n| n.value.clone()).unwrap();
        let limit = store.record_limit(&contacts, &code).unwrap();
        let environment =
            crate::recalc::record_environment("limited", crate::recalc::random_uuid());
        let mut answers = BTreeMap::new();
        for record in &store.node(&[&contacts, &[5]]).unwrap().children {
            let row = crate::recalc::texts(record);
            let first_name = row.get(&1).cloned().unwrap_or_default();
            answers.insert(first_name, limit.allows(&row, &environment));
        }
        assert_eq!(
            answers,
            BTreeMap::from([("Allowed".to_owned(), true), ("Denied".to_owned(), false)])
        );
    }

    /// The calculations of a store FileMaker Server's engine serves compile
    /// text without the location functions (script oracle s48), as its
    /// view and write limits and stored recalculation read them; one served
    /// privately to a desktop client keeps them, as FileMaker Pro has them
    /// (#79).
    #[test]
    fn only_a_desktop_clients_store_compiles_the_location_functions() {
        use om_formula::Names;
        let location = om_formula::catalog::find_builtin("Location").unwrap().id;
        for (desktop, known) in [(false, false), (true, true)] {
            let mut store = store_of(OwnedNode::default());
            if desktop {
                store.serve_desktop();
            }
            let compiles = store.with_calc_inputs(&uint_segment(0x8001), |_, calc| {
                calc.names().builtin_known(location)
            });
            assert_eq!(compiles, Some(known), "desktop={desktop}");
        }
    }

    /// A store holding `root`, with nothing unsaved and no lock.
    fn store_of(root: OwnedNode) -> Store {
        let derived = Derived::new(&TreeView::new(&root, 0), Catalog::default(), "")
            .expect("the file engine's tree never fails a read");
        Store {
            root,
            version: 0,
            derived: Arc::new(derived),
            name: String::new(),
            basis: HashMap::new(),
            superblock: Vec::new(),
            dirty: false,
            saves: 0,
            #[cfg(debug_assertions)]
            fail_commit_saves: false,
            #[cfg(debug_assertions)]
            save_fault: None,
            unknown: None,
            generation: 0,
            accounts_at: 0,
            waiting: Waiting::default(),
            verified: (0, BTreeMap::new()),
            lock: None,
            file: None,
            read_only: None,
        }
    }

    /// A save's new copy holds the whole database: it is owner-only from
    /// the moment it exists, whatever the umask, and takes the old file's
    /// permissions only once it is complete. With no old file it is a new
    /// file from the start.
    #[cfg(unix)]
    #[test]
    fn a_saves_copy_is_owner_only_until_it_takes_the_files_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("om-store-save-modes-{}.fmp12", std::process::id()));
        let mode = |path: &Path| std::fs::symlink_metadata(path).map(|m| m.permissions().mode());
        let mut store = store_of(OwnedNode::default());
        store.superblock = vec![0; 4096];
        store.superblock[0x0f..0x14].copy_from_slice(b"HBAM7");
        store.superblock[0x14..0x18].copy_from_slice(&4096u32.to_be_bytes());
        std::fs::write(&path, b"old").unwrap();
        let at = FileAt::open(&path).unwrap();
        let save = |store: &mut Store| {
            store.dirty = true;
            let mut copy = None;
            let saved = store.save_observed(&at, &mut |c| copy = mode(c).ok());
            (
                saved.ok(),
                copy.map(|m| m & 0o777),
                mode(&path).ok().map(|m| m & 0o777),
            )
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        // Under this umask a copy made with a new file's permissions is
        // 0644, readable by everyone.
        let (replaced, new) = crate::with_umask(0o022, || {
            let replaced = save(&mut store);
            std::fs::remove_file(&path).unwrap();
            (replaced, save(&mut store))
        });
        let _ = std::fs::remove_file(&path);
        assert_eq!(replaced, (Some(true), Some(0o600), Some(0o640)));
        assert_eq!(new, (Some(true), Some(0o644), Some(0o644)));
    }

    /// A hosted file is saved only where its path still leads to the file
    /// the store was read from (#468). With its folder moved away and a link
    /// to another folder in its place, the save fails, writes nowhere, and
    /// the changes stay unsaved; so it does when that folder's entry is a
    /// link back to the file. With the link leading to the moved folder,
    /// which holds the same file, the save goes there.
    #[cfg(unix)]
    #[test]
    fn a_save_is_refused_where_a_link_leads_away_from_the_hosted_file() {
        use std::os::unix::fs::MetadataExt;
        let base = std::env::temp_dir().join(format!("om-store-anchor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (databases, moved, elsewhere) = (
            base.join("Databases"),
            base.join("moved"),
            base.join("elsewhere"),
        );
        std::fs::create_dir_all(&databases).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let path = databases.join("Tasks.fmp12");
        std::fs::write(&path, b"old").unwrap();
        let at = FileAt::open(&path).unwrap();
        let mut store = store_of(OwnedNode::default());
        let read = std::fs::metadata(&path).unwrap();
        store.set_file(Some((read.dev(), read.ino())));
        store.superblock = vec![0; 4096];
        store.superblock[0x0f..0x14].copy_from_slice(b"HBAM7");
        store.superblock[0x14..0x18].copy_from_slice(&4096u32.to_be_bytes());
        std::fs::rename(&databases, &moved).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &databases).unwrap();
        store.dirty = true;
        let refused = store.save(&at);
        let unsaved = store.dirty;
        let kept = std::fs::read(moved.join("Tasks.fmp12")).ok();
        let followed = std::fs::read_dir(&elsewhere).map(|d| d.count()).ok();
        // The other folder's entry, a link back to the file.
        std::os::unix::fs::symlink(moved.join("Tasks.fmp12"), elsewhere.join("Tasks.fmp12"))
            .unwrap();
        let through_link = store.save(&at);
        let link_kept = std::fs::symlink_metadata(elsewhere.join("Tasks.fmp12"))
            .is_ok_and(|m| m.file_type().is_symlink());
        let still_old = std::fs::read(moved.join("Tasks.fmp12")).ok();
        // The administrator's move: the path leads to the same file again.
        std::fs::remove_file(&databases).unwrap();
        std::os::unix::fs::symlink(&moved, &databases).unwrap();
        let saved = store.save(&at);
        let published = std::fs::metadata(&path).ok().map(|m| (m.dev(), m.ino()));
        let written = std::fs::read(moved.join("Tasks.fmp12"))
            .map(|b| b.len())
            .ok();
        let _ = std::fs::remove_dir_all(&base);
        assert!(refused.is_err(), "{refused:?}");
        assert!(unsaved, "the changes wait in memory");
        assert_eq!(kept.as_deref(), Some(&b"old"[..]));
        assert_eq!(followed, Some(0), "nothing written through the link");
        assert!(through_link.is_err(), "{through_link:?}");
        assert!(link_kept, "the link back is not saved over");
        assert_eq!(still_old.as_deref(), Some(&b"old"[..]));
        assert_eq!(saved.ok(), Some(true));
        assert_ne!(store.file(), Some((read.dev(), read.ino())));
        assert_eq!(store.file(), published);
        assert!(written.is_some_and(|n| n > 3), "{written:?}");
    }
    #[test]
    fn serials_advance_their_last_number() {
        assert_eq!(increment_serial("698047", 1), "698048");
        assert_eq!(increment_serial("INV0099", 1), "INV0100");
        assert_eq!(increment_serial("A9-B", 1), "A10-B");
        assert_eq!(increment_serial("1", 10), "11");
        assert_eq!(increment_serial("X", 1), "X1");
        assert_eq!(increment_serial("INV0999-2026", 7), "INV0999-2033");
        assert_eq!(increment_serial("98", 2147483647), "2147483745");
        // However long the number, it advances: a value left as it was
        // would go to two records.
        let nines = "9".repeat(60);
        assert_eq!(
            increment_serial(&format!("A{nines}"), 1),
            format!("A1{}", "0".repeat(60))
        );
        assert_eq!(increment_serial("5", -2), "3");
        assert_eq!(increment_serial("1", -2), "0");
    }

    /// Serial numbers are handed out under the store's write lock, as op72
    /// takes it (`writes::get_serial`) and a server script's commit does:
    /// two clients reserving at once never get one value, and a value whose
    /// record was never committed (a reverted record) is not handed out
    /// again. The next value stays in the field's `0c` as text, where
    /// FileMaker reads it.
    #[test]
    fn two_clients_never_get_the_same_serial_number() {
        let (t, f) = ([0x80, 0x02], [0x04]);
        let mut definition = vec![0; 26];
        definition[..2].copy_from_slice(&[1, 1]);
        definition[8..12].copy_from_slice(&2u32.to_be_bytes());
        definition[16..20].copy_from_slice(&1u32.to_be_bytes());
        let mut root = OwnedNode::default();
        root.put(
            &[&t, &[3], &[5], &f, &[2]],
            OwnedNode::leaf(Vec::new(), definition.clone()),
        );
        root.put(
            &[&t, &[3], &[5], &f, &[keys::SERIAL_VALUE]],
            OwnedNode::leaf(Vec::new(), om_scsu::encode_scsu("INV0098")),
        );
        let mut store = store_of(root);
        Arc::make_mut(&mut store.derived)
            .catalog_mut()
            .tables
            .push(om_hbam::Table {
                id: 0x8002,
                name: "Invoices".into(),
                fields: vec![om_hbam::Field {
                    id: 4,
                    name: "Number".into(),
                    kind: om_hbam::FieldKind::Normal,
                    data_type: DataType::Text,
                    options: definition,
                }],
            });
        let store = std::sync::RwLock::new(store);
        let reserve = || {
            (0..100)
                .map(|_| {
                    let mut store = store.write().unwrap();
                    store.serial_value(&t, &f, None, Some(1)).unwrap()
                })
                .collect::<Vec<_>>()
        };
        let handed: Vec<String> = std::thread::scope(|scope| {
            let clients = [scope.spawn(reserve), scope.spawn(reserve)];
            clients
                .into_iter()
                .flat_map(|client| client.join().unwrap())
                .collect()
        });
        let handed: BTreeSet<String> = handed.into_iter().collect();
        let expected: BTreeSet<String> = (98..298).map(|n| format!("INV{n:04}")).collect();
        assert_eq!(handed, expected);
        let store = store.into_inner().unwrap();
        let next = store.node(&[&t, &[3], &[5], &f, &[keys::SERIAL_VALUE]]);
        assert_eq!(next.and_then(cell_text).as_deref(), Some("INV0298"));
    }

    /// A field's repetition cells are `F` then the byte n, but not a key
    /// another field of the same work owns: deleted in the same edit, it
    /// is gone from the catalog, and its own deletion releases its
    /// container data.
    #[test]
    fn repetitions_leave_other_fields_cells() {
        let store = store_of(OwnedNode::default());
        let others: BTreeSet<Vec<u8>> = [vec![1], vec![1, 2]].into();
        let cells = store.repetition_cells(&[0x80, 0x02], &[1], &others);
        assert_eq!(cells.len(), 253);
        assert!(cells.contains(&vec![1, 3]) && !cells.contains(&vec![1, 2]));
        assert_eq!(
            store
                .repetition_cells(&[0x80, 0x02], &[1], &BTreeSet::new())
                .len(),
            254
        );
    }

    #[test]
    fn deleted_tables_take_their_libraries() {
        // A table with a container library member, and another table.
        let mut root = OwnedNode::default();
        let member = [&[0x80, 0x02][..], &[LIBRARY], &[5], &[1], &[0x12]];
        root.put(&member, OwnedNode::leaf(Vec::new(), vec![0, 0, 0, 1]));
        root.put(
            &[&[0x80, 0x03], &[0]],
            OwnedNode::leaf(Vec::new(), vec![1, 1]),
        );
        let mut store = store_of(root);
        // DeleteTable (schema-path.md 3.4): the library goes with [T].
        let released = store.apply(vec![Write::Remove {
            path: vec![vec![0x80, 0x02]],
        }]);
        assert_eq!(released, vec![vec![vec![0x80, 0x02], vec![LIBRARY]]]);
        assert!(store.node(&member).is_none());
        assert!(store.node(&[&[0x80, 0x03]]).is_some());
        // Removing a catalog member is no table's deletion.
        let released = store.apply(vec![Write::Remove {
            path: vec![vec![3], vec![0x10], vec![5], vec![0x80, 0x02]],
        }]);
        assert!(released.is_empty());
    }

    #[test]
    fn member_keys_are_issued_from_the_catalog() {
        use om_query::Source;
        // live:s4: [4, 1, 00] = 01 04 and fc = 01 0d give 05/0e, then 06/0f;
        // live:s4b: [3, 16, 1, 00] = 02 80 05 and fc = 01 02 give 80 06/03.
        let mut root = OwnedNode::default();
        root.put(&[&[4], &[1], &[0]], OwnedNode::leaf(Vec::new(), vec![1, 4]));
        root.put(
            &[&[4], &[1], &[0xfc]],
            OwnedNode::leaf(Vec::new(), vec![1, 0x0d]),
        );
        let tables: [&[u8]; 3] = [&[3], &[0x10], &[1]];
        root.put(
            &[tables[0], tables[1], tables[2], &[0]],
            OwnedNode::leaf(Vec::new(), vec![2, 0x80, 0x05]),
        );
        root.put(
            &[tables[0], tables[1], tables[2], &[0xfc]],
            OwnedNode::leaf(Vec::new(), vec![1, 2]),
        );
        let mut store = store_of(root);
        let layouts = vec![vec![4]];
        let generate = |catalog: &[Vec<u8>]| store.generate_member_key(catalog).unwrap();
        assert_eq!(generate(&layouts), (vec![1, 5], vec![1, 0x0e]));
        assert_eq!(generate(&layouts), (vec![1, 6], vec![1, 0x0f]));
        assert_eq!(
            generate(&[vec![3], vec![0x10]]),
            (vec![2, 0x80, 0x06], vec![1, 3])
        );
        assert_eq!(generate(&layouts), (vec![1, 7], vec![1, 0x10]));
        // The issued values go into the indexes (live:s4 g_41_after, s5).
        assert!(store.has_issued_member_keys());
        let issued = store.take_issued_member_keys();
        store.write_member_keys(issued);
        assert!(!store.has_issued_member_keys());
        let value = |path: &[&[u8]]| store.node(path).unwrap().value.clone().unwrap();
        assert_eq!(value(&[&[4], &[1], &[0]]), [1, 7]);
        assert_eq!(value(&[&[4], &[1], &[0xfc]]), [1, 0x10]);
        assert_eq!(value(&[&[3], &[0x10], &[1], &[0]]), [2, 0x80, 0x06]);
        assert_eq!(
            store.generate_member_key(&layouts).unwrap(),
            (vec![1, 8], vec![1, 0x11])
        );
        // A catalog whose last issued id is the largest has no next one.
        let scripts: [&[u8]; 3] = [&[0x11], &[1], &[0]];
        let largest = modcount::serial_leaf(0, u64::MAX);
        store.root.put(&scripts, largest);
        assert_eq!(store.generate_member_key(&[vec![0x11]]), None);
        let issued = store.take_issued_member_keys();
        assert!(issued.iter().all(|(catalog, _)| catalog != &[vec![0x11]]));
    }

    /// A library whose last member id or member count the file holds at the
    /// largest a serial key holds (#361): op88 and op87 are refused through
    /// `library_refusal`, as the script's container store and its reference
    /// counting are, and change nothing.
    #[test]
    fn library_counters_at_the_largest_are_refused() {
        let data = file_data;
        let table: &[u8] = &[0x80, 0x01];
        let path: [&[u8]; 2] = [table, &[LIBRARY]];
        let mut store = store_of(OwnedNode::default());
        let stored = store.store_binary(&path, data(b"a"), || [1; 16]);
        assert_eq!(stored, Some(Stored::New(vec![1])));
        let largest = |key: u8| modcount::serial_leaf(key, u64::MAX);
        store.root.put(&[table, &[LIBRARY], &[1], &[0]], largest(0));
        // Reusing data advances its member's counters, not the last issued id.
        assert_eq!(
            store.store_binary(&path, data(b"a"), || [2; 16]),
            Some(Stored::Existing(vec![1]))
        );
        assert_eq!(
            library::reference_count(store.library_member(&path, &[1]).unwrap()),
            2
        );
        store.root.put(
            &[table, &[LIBRARY], &[5], &[1], &[MOD_COUNT]],
            largest(MOD_COUNT),
        );
        let before = store.root.clone();
        // A new member, the member reused, its count adjusted.
        assert_eq!(store.store_binary(&path, data(b"b"), || [2; 16]), None);
        assert_eq!(store.store_binary(&path, data(b"a"), || [2; 16]), None);
        assert_eq!(store.adjust_reference_count(&path, &[1], 1), None);
        assert_eq!(store.root, before);
    }

    /// Container data of one `FILE` stream holding `bytes`.
    fn file_data(bytes: &[u8]) -> BinaryData {
        use om_data::containers::{md5, stream_node};
        let mut directory = 1u16.to_be_bytes().to_vec();
        directory.extend(b"FILE");
        directory.extend((bytes.len() as u32).to_be_bytes());
        let md5 = md5([(&b"FILE"[..], bytes)]);
        let keys = vec![
            OwnedNode::leaf(vec![2], directory),
            OwnedNode::leaf(vec![4], md5.to_vec()),
            stream_node(b"FILE", bytes),
        ];
        BinaryData::from_tree(keys, md5)
    }

    /// A container's repetitions cut (`0x4 | 0x8`, schema-path.md 7.2):
    /// the repetitions above the new maximum go and release their library
    /// references, while the first keeps its cell and the member it shares
    /// with them keeps its one reference.
    #[test]
    fn a_cut_container_releases_only_the_repetitions_it_drops() {
        let table: &[u8] = &[0x80, 0x01];
        let path: [&[u8]; 2] = [table, &[LIBRARY]];
        let mut store = store_of(OwnedNode::default());
        for _ in 0..2 {
            store.store_binary(&path, file_data(b"a"), || [1; 16]);
        }
        let count =
            |store: &Store| library::reference_count(store.library_member(&path, &[1]).unwrap());
        assert_eq!(count(&store), 2);
        let reference = om_scripts::text_node(
            &[],
            &om_data::containers::reference_text(&serial::encode(1)),
        );
        let record = serial::encode(1);
        let (first, second) = (vec![2u8], vec![2u8, 2]);
        for cell in [&first, &second] {
            store
                .root
                .put(&[table, &[keys::RECORDS], &record, cell], reference.clone());
        }
        // Cut to one repetition: from repetition 2 up.
        let mut removed = BTreeMap::new();
        store.drop_field_values(
            None,
            table,
            &first,
            (2, true),
            &BTreeSet::new(),
            &mut removed,
        );
        let naming = store.naming_cells(
            table,
            &[(first.clone(), deferred::FEWER | deferred::CONTAINER)],
            &BTreeSet::new(),
        );
        store.release_containers(None, table, removed, &naming);
        let cell = |cell: &[u8]| {
            store
                .node(&[table, &[keys::RECORDS], &record, cell])
                .is_some()
        };
        assert!(cell(&first) && !cell(&second));
        assert_eq!(count(&store), 1);
    }

    /// Repetition n of field F is the cell `F * 256 + n`, F from the file
    /// (#361): the largest field id whose repetitions have keys reads its
    /// 255th from the cell `u64::MAX`; past it only the first repetition
    /// has a cell, and the others read as empty.
    #[test]
    fn repetitions_of_the_largest_field_ids() {
        let text = |id: u64, value: &str| om_scripts::text_node(&uint_segment(id), value);
        let largest = u64::MAX / 256;
        let record = OwnedNode::list(
            vec![1],
            vec![text(largest, "first"), text(u64::MAX, "last")],
        );
        let texts = field_texts(&record, &uint_segment(largest), 255);
        assert_eq!((texts[0].as_str(), texts[254].as_str()), ("first", "last"));
        let past = OwnedNode::list(vec![1], vec![text(largest + 1, "only")]);
        assert_eq!(
            field_texts(&past, &uint_segment(largest + 1), 3),
            ["only", "", ""]
        );
    }
}
