//! What an operation runs against ([`super::run`]): the file's tree as the
//! engine holds it ([`Hold`], [`Transaction`]) and its lock table
//! ([`Locks`]). Both keep what the engine needs to take the operation's
//! effects back when they cannot be stored.

use std::borrow::Cow;
use std::ops::Bound;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use om_data::rules::RecordEnvironment;
use om_hbam::OwnedNode;
use om_protocol::write::{Conflict, KeyPath};

use super::view::{Child, Direction, Head, ReadError, Step, Version, View};
use crate::derived::Derived;
use crate::library::{BinaryData, Stored};
use crate::locks::{LockTable, LockUser};
use crate::store::{IndexId, Journal, Store};

/// What the engine holds of a file for an operation: what the operation
/// needs, and no more (`FileEngine::submit`).
pub enum Hold<'h, 'a> {
    /// Nothing of the tree: only the lock table (op61, op62, op65, a
    /// server script's lock and its release, a session's end).
    LockTable,
    /// The tree, for reading (op60).
    View(&'h Store),
    /// A transaction on the tree (op70, op71, op72, op13, op73, op86's
    /// writes and a find's indexes, op87, op88, op240, a server script's
    /// commit, Set Next Serial Value, the account edits, deferred field work
    /// and a saved found set).
    Transaction(&'h mut Transaction<'a>),
}

/// A transaction on a file's tree, over the file engine's store: what an
/// operation reads and the changes it makes, kept so that the engine can
/// store them or take them back (docs/server-storage.md 5, "The
/// operations' transaction").
///
/// It is a view ([`View`]) of the version the operation started on with
/// the operation's own writes applied, its derived state
/// ([`Self::derived`]), and a closed set of writes, those the store
/// engine's entries hold (`om_store::tree::Edit`): put a node
/// ([`Self::put`]), remove one with everything under it
/// ([`Self::remove`]), replace a subtree ([`Self::replace`]). An operation
/// moved onto it reads and changes the tree through these alone.
///
/// The operations not moved yet use the `Store` methods it still exposes,
/// and `view` for the state derived from the tree (a privilege set's
/// hidden fields and record limits). Every change goes
/// into one `Journal`, which keeps what each replaced: a commit's record
/// writes (with a server script's container data and reference counts), an
/// authoring edit's and an account edit's catalog members, Set Next Serial
/// Value's value, a window's saved found set, op71's last issued id, op72's
/// serial value, op13's and op86's catalog writes (moved into it, not
/// copied), the member keys op86 writes, a find's stored indexes, op87's
/// and op88's library entries, and op73's records, work list, global
/// values, indexes, library entries and basis, which the deferred field
/// work finished at opening keeps the same way.
pub struct Transaction<'a> {
    pub(super) store: &'a mut Store,
    /// Every change the operation made to the tree, which the engine
    /// stores or takes back.
    pub(super) journal: Journal,
    /// Whether the operation began a commit ([`Self::begin_commit`]), which
    /// is saved even if it writes nothing.
    pub(super) committing: bool,
    /// Whether op71 reserved ids, which are saved before they are answered.
    pub(super) reserved: bool,
    /// Log lines that say a change was made, written once it is stored.
    pub(super) stored_lines: Vec<String>,
    /// The container libraries deleted tables took with them
    /// ([`Self::release_library`]), whose thumbnails the engine forgets.
    pub(super) released_libraries: Vec<Vec<Vec<u8>>>,
    /// The libraries of tables a server script's commit left without
    /// records ([`Self::drop_emptied_library`]), whose thumbnails the
    /// engine forgets once the commit is stored.
    pub(super) emptied_libraries: Vec<Vec<Vec<u8>>>,
    /// Fields an authoring edit added, counted as computed once the edit
    /// is stored ([`Self::add_field_basis_once_stored`]).
    pub(super) field_bases: Vec<(Vec<u8>, u64)>,
    /// Whether the run began writing member keys
    /// ([`Self::wrote_member_keys`]).
    pub(super) wrote_member_keys: bool,
    /// Whether a write reached what the derived state reads, which is read
    /// again before the derived state is next used ([`Self::derived`]) or
    /// the operation ends ([`Self::settle`]).
    schema_pending: bool,
    /// The check of its writes, in tests (`super::checking`).
    #[cfg(debug_assertions)]
    pub(super) check: Option<Box<super::checking::Check>>,
}

impl<'a> Transaction<'a> {
    pub(super) fn new(store: &'a mut Store) -> Self {
        let journal = store.begin_commit();
        Transaction {
            store,
            journal,
            committing: false,
            reserved: false,
            stored_lines: Vec::new(),
            released_libraries: Vec::new(),
            emptied_libraries: Vec::new(),
            field_bases: Vec::new(),
            wrote_member_keys: false,
            schema_pending: false,
            #[cfg(debug_assertions)]
            check: None,
        }
    }

    /// Check the operation's reads and writes (`super::checking`).
    #[cfg(debug_assertions)]
    pub(super) fn checked(&mut self) {
        self.check = Some(Box::new(super::checking::Check::new(self.store)));
    }

    /// The operation changed the tree through the store's own helpers.
    fn by_store(&mut self) {
        #[cfg(debug_assertions)]
        if let Some(check) = &mut self.check {
            check.by_store = true;
        }
    }

    /// Put `node` (with its children) at `path`, replacing whatever is
    /// there and everything under it (`om_store::tree::Edit::Put`, then a
    /// put of each node under it): missing parents become lists, and a
    /// parent that holds an empty value becomes an empty list, as
    /// `OwnedNode::put` puts it. A put the file cannot hold there
    /// (`om_hbam::writer::put_refusals`) is refused, for a log line, and
    /// changes nothing.
    pub fn replace(&mut self, path: Vec<Vec<u8>>, node: OwnedNode) -> Result<(), String> {
        #[cfg(debug_assertions)]
        let recorded = self.check.as_ref().map(|_| node.clone());
        let feeds = self.store.write_in(&mut self.journal, &path, Some(node))?;
        self.schema_pending |= feeds;
        #[cfg(debug_assertions)]
        if let (Some(check), Some(node)) = (&mut self.check, recorded) {
            check.wrote(&path, Some(&node));
        }
        Ok(())
    }

    /// Put `node`, without children, at `path` ([`Self::replace`]).
    pub fn put(&mut self, path: Vec<Vec<u8>>, node: OwnedNode) -> Result<(), String> {
        let node = OwnedNode {
            children: Vec::new(),
            ..node
        };
        self.replace(path, node)
    }

    /// Remove the node at `path` and everything under it
    /// (`om_store::tree::Edit::Delete`).
    pub fn remove(&mut self, path: Vec<Vec<u8>>) -> Result<(), String> {
        let feeds = self.store.write_in(&mut self.journal, &path, None)?;
        self.schema_pending |= feeds;
        #[cfg(debug_assertions)]
        if let Some(check) = &mut self.check {
            check.wrote(&path, None);
        }
        Ok(())
    }

    /// The state derived from the tree as the transaction has it (the
    /// catalog, the accounts, the locale), read again after writes that
    /// reach it.
    pub fn derived(&mut self) -> Arc<Derived> {
        self.settle();
        self.store.derived().clone()
    }

    /// Read the derived state again after writes that reached it, the
    /// state before them kept to take back: before the derived state is
    /// used, and when the operation ends (`FileEngine::submit`).
    pub(super) fn settle(&mut self) {
        if std::mem::take(&mut self.schema_pending) {
            self.store.schema_changed_in(&mut self.journal);
        }
    }

    /// The tree as the transaction has it, for state derived from it.
    pub fn view(&self) -> &Store {
        self.store
    }

    /// Whether the file may not be written.
    pub fn is_read_only(&self) -> bool {
        self.store.is_read_only()
    }

    pub fn has_table(&self, table: &[u8]) -> bool {
        self.store.has_table(table)
    }

    pub fn has_record(&self, table: &[u8], record: u64) -> bool {
        self.store.has_record(table, record)
    }

    pub fn last_record_id(&self, table: &[u8]) -> u64 {
        self.store.last_record_id(table)
    }

    /// The node at `path` with everything under it, from the store: for
    /// the operations not moved onto the transaction's reads ([`View`]).
    pub fn store_node(&self, path: &[&[u8]]) -> Option<&OwnedNode> {
        self.store.node(path)
    }

    /// A write the operation needed was refused ([`Self::replace`]): the
    /// engine takes back the rest of its commit and fails its save, as
    /// content the file cannot hold.
    pub fn refuse(&mut self, reason: String) {
        self.journal.refuse(reason);
    }

    /// GenerateMemberKey's issued keys are being written (op86).
    pub fn wrote_member_keys(&mut self) {
        self.wrote_member_keys = true;
    }

    /// A deleted table took the library at `path` with it, whose thumbnails
    /// the engine forgets once the change is stored.
    pub fn release_library(&mut self, path: Vec<Vec<u8>>) {
        self.released_libraries.push(path);
    }

    /// The operation reserved record ids (op71), which the engine saves
    /// before they are answered, or takes back byte for byte when the save
    /// fails; a reservation is not a commit.
    pub fn reserved_ids(&mut self) {
        self.reserved = true;
    }

    /// Start the commit the record writes go into, which the engine saves
    /// whole or takes back (`Store::save_commit`), even if it writes
    /// nothing.
    pub fn begin_commit(&mut self) {
        self.committing = true;
    }

    /// Write record `id` of `table` (`None` deletes it) within the commit
    /// (`Store::write_record_in`).
    pub fn write_record(&mut self, table: &[u8], id: u64, record: Option<OwnedNode>) {
        self.by_store();
        self.committing = true;
        self.store
            .write_record_in(&mut self.journal, table, id, record);
        #[cfg(test)]
        if PANIC_WRITING_RECORDS.with(std::cell::Cell::get) {
            panic!("a test's panic after writing a record");
        }
    }

    /// Store container data a server script set into a cell of a record of
    /// `table` in the table's library, within the commit
    /// (`Store::store_container_in`), a new member's unique id from
    /// `unique_id`. Returns the member, its reference counted.
    pub fn store_container(
        &mut self,
        table: &[u8],
        data: BinaryData,
        unique_id: &mut dyn FnMut() -> [u8; 16],
    ) -> Vec<u8> {
        self.by_store();
        self.committing = true;
        self.store
            .store_container_in(&mut self.journal, table, data, unique_id)
    }

    /// Count the container references of a server script's record of
    /// `table`, `old` as stored and `new` as it will be, within the commit
    /// (`Store::count_containers_in`).
    pub fn count_containers(
        &mut self,
        table: &[u8],
        old: Option<&OwnedNode>,
        new: Option<&OwnedNode>,
        counted: &[Vec<u8>],
    ) {
        self.by_store();
        self.committing = true;
        self.store
            .count_containers_in(&mut self.journal, table, old, new, counted);
    }

    /// Release the container references of the records the commit deleted
    /// (`Store::release_deleted_record_containers`).
    pub fn release_deleted_record_containers(&mut self) {
        self.by_store();
        self.committing = true;
        self.store
            .release_deleted_record_containers(&mut self.journal);
    }

    /// Delete All Records or Truncate Table: drop the library of `table`
    /// when the commit left it without records
    /// (`Store::drop_emptied_library_in`), its thumbnails forgotten once
    /// the commit is stored. Whether it did.
    pub fn drop_emptied_library(&mut self, table: &[u8]) -> bool {
        self.by_store();
        self.committing = true;
        let dropped = self.store.drop_emptied_library_in(&mut self.journal, table);
        if dropped {
            self.emptied_libraries
                .push(vec![table.to_vec(), vec![crate::library::LIBRARY]]);
        }
        dropped
    }

    /// Run `work`: when it panics, the operation's changes are put back
    /// before the panic goes on, as `Store::guarded` does, so the store a
    /// poisoned lock hands to the next request is the one before the
    /// operation, counted as changed since its last save
    /// (`Store::roll_back_unwound`).
    pub fn guarded<T>(&mut self, work: impl FnOnce(&mut Self) -> T) -> T {
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(self)));
        match done {
            Ok(value) => value,
            Err(panic) => {
                let journal = std::mem::replace(&mut self.journal, self.store.begin_commit());
                self.store.roll_back_unwound(journal);
                // Begun again from the store as it was put back, so that the
                // engine, rolling this one back, leaves it so.
                self.journal = self.store.begin_commit();
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// Log `line` once the transaction is stored, and not if it is not.
    pub fn once_stored(&mut self, line: String) {
        self.stored_lines.push(line);
    }

    /// Indexes finds built on demand (`Store::store_indexes_in`).
    pub fn store_indexes(&mut self, indexes: Vec<IndexId>) {
        if !indexes.is_empty() {
            self.by_store();
        }
        #[cfg(test)]
        if PANIC_STORING_INDEXES.with(std::cell::Cell::get) {
            panic!("a test's panic while storing indexes");
        }
        self.store.store_indexes_in(&mut self.journal, indexes);
    }

    /// Replace catalog members the schema does not hold, such as a script
    /// and the script catalog's index, within the commit
    /// (`Store::replace_members_in`).
    pub fn replace_members(&mut self, members: Vec<(Vec<Vec<u8>>, OwnedNode)>) {
        self.by_store();
        self.committing = true;
        self.store.replace_members_in(&mut self.journal, members);
    }

    /// Replace the schema node at `path` within the commit, the schema read
    /// again (`Store::replace_schema_in`).
    pub fn replace_schema(&mut self, path: Vec<Vec<u8>>, node: OwnedNode) {
        self.by_store();
        self.committing = true;
        self.store.replace_schema_in(&mut self.journal, path, node);
    }

    /// Remove the schema node at `path` within the commit
    /// (`Store::remove_schema_in`).
    pub fn remove_schema(&mut self, path: Vec<Vec<u8>>) {
        self.by_store();
        self.committing = true;
        self.store.remove_schema_in(&mut self.journal, path);
    }

    /// Count field `field` of `table`, which the commit adds, as computed
    /// once the commit is stored (`Store::add_field_basis`), and not if it
    /// is not.
    pub fn add_field_basis_once_stored(&mut self, table: Vec<u8>, field: u64) {
        self.field_bases.push((table, field));
    }

    /// op88: store container data in the library at `path`
    /// (`Store::store_binary_in`).
    pub fn store_binary(
        &mut self,
        path: &[&[u8]],
        data: BinaryData,
        unique_id: impl FnMut() -> [u8; 16],
    ) -> Option<Stored> {
        self.by_store();
        self.store
            .store_binary_in(&mut self.journal, path, data, unique_id)
    }

    /// op87: add `delta` to a library member's reference count
    /// (`Store::adjust_reference_count_in`).
    pub fn adjust_reference_count(
        &mut self,
        path: &[&[u8]],
        member: &[u8],
        delta: i32,
    ) -> Option<u32> {
        self.by_store();
        self.store
            .adjust_reference_count_in(&mut self.journal, path, member, delta)
    }

    /// op73: a table's deferred field work (`Store::update_table`), each
    /// record's environment for a recalculation from `environment`, and
    /// the unique ids of container data it stores from `unique_id`, every
    /// change in the journal.
    pub fn update_table(
        &mut self,
        table: &[u8],
        environment: &mut dyn FnMut() -> RecordEnvironment,
        unique_id: &mut dyn FnMut() -> [u8; 16],
    ) -> Result<bool, &'static str> {
        self.by_store();
        self.store
            .update_table(Some(&mut self.journal), table, environment, unique_id)
    }

    /// The deferred field work the file records as pending, as op73 does
    /// it for each table (`Store::recover`), every change in the journal:
    /// whether it changed the file. Work that cannot be done is kept, and
    /// the file hosted read-only.
    pub fn recover(
        &mut self,
        environment: &mut dyn FnMut() -> RecordEnvironment,
        unique_id: &mut dyn FnMut() -> [u8; 16],
    ) -> bool {
        self.by_store();
        let recovered = self
            .store
            .recover(Some(&mut self.journal), environment, unique_id);
        #[cfg(test)]
        if PANIC_RECOVERING.with(std::cell::Cell::get) {
            panic!("a test's panic after finishing deferred work");
        }
        recovered
    }
}

/// The transaction's reads: the tree as the operation's writes left it,
/// at the version it started on. In tests each answer is checked against
/// the starting tree with the writes laid over it (`super::checking`).
impl View for Transaction<'_> {
    fn version(&self) -> Version {
        self.store.version()
    }

    fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
        let ours = self.store.tree_view().head(path);
        #[cfg(debug_assertions)]
        if let Some(check) = &self.check {
            let owned = ours.clone().map(Head::into_owned);
            check.read("node", path, &owned, |view| {
                view.node(path).ok().flatten().map(Head::into_owned)
            });
        }
        Ok(ours)
    }

    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError> {
        let ours = self.store.tree_view().lend(path, limit);
        #[cfg(debug_assertions)]
        if let Some(check) = &self.check {
            let owned = ours.clone().map(|node| node.cloned());
            check.read("subtree", path, &owned, |view| {
                view.subtree(path, limit)
                    .map(|node| node.map(Cow::into_owned))
            });
        }
        Ok(ours?.map(Cow::Borrowed))
    }

    fn children_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'v>>,
    ) -> Result<bool, ReadError> {
        let given = out.len();
        let more = self
            .store
            .tree_view()
            .children_into(path, from, direction, max, out)?;
        #[cfg(debug_assertions)]
        if let Some(check) = &self.check {
            let owned = (owned_children(&out[given..]), more);
            check.read("listing", path, &owned, |view| {
                let mut page = Vec::new();
                let more = view
                    .children_page(path, from, direction, max, &mut page)
                    .unwrap_or_default();
                (owned_children(&page), more)
            });
        }
        #[cfg(not(debug_assertions))]
        let _ = given;
        Ok(more)
    }

    fn subtrees_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        limit: usize,
        out: &mut Vec<Cow<'v, OwnedNode>>,
    ) -> Result<bool, ReadError> {
        let given = out.len();
        let more = self
            .store
            .tree_view()
            .subtrees_into(path, from, direction, max, limit, out);
        #[cfg(debug_assertions)]
        if let Some(check) = &self.check {
            let owned = more.clone().map(|more| {
                let page: Vec<OwnedNode> = out[given..]
                    .iter()
                    .map(|n| n.clone().into_owned())
                    .collect();
                (page, more)
            });
            check.read("subtrees", path, &owned, |view| {
                let mut page = Vec::new();
                view.subtrees_page(path, from, direction, max, limit, &mut page)
                    .map(|more| (page.into_iter().map(Cow::into_owned).collect(), more))
            });
        }
        #[cfg(not(debug_assertions))]
        let _ = given;
        more
    }

    fn walk_page<'v>(
        &'v self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'v>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        let given = out.len();
        let resume = self.store.tree_view().walk_into(path, after, max, out);
        #[cfg(debug_assertions)]
        if let Some(check) = &self.check {
            let owned = (owned_steps(&out[given..]), resume.clone());
            check.read("walk", path, &owned, |view| {
                let mut page = Vec::new();
                let resume = view.walk_page(path, after, max, &mut page).ok().flatten();
                (owned_steps(&page), resume)
            });
        }
        #[cfg(not(debug_assertions))]
        let _ = given;
        Ok(resume)
    }

    fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
        self.store.tree_view().changed_since(path, since)
    }
}

#[cfg(debug_assertions)]
fn owned_children(children: &[Child<'_>]) -> Vec<(Vec<u8>, Head<'static>)> {
    children
        .iter()
        .map(|child| (child.segment.to_vec(), child.head.clone().into_owned()))
        .collect()
}

#[cfg(debug_assertions)]
fn owned_steps(steps: &[Step<'_>]) -> Vec<(usize, Vec<u8>, Head<'static>)> {
    steps
        .iter()
        .map(|step| {
            (
                step.depth,
                step.segment.to_vec(),
                step.head.clone().into_owned(),
            )
        })
        .collect()
}

#[cfg(test)]
thread_local! {
    /// Makes [`Transaction::store_indexes`] panic on this thread
    /// (`engine::tests`).
    pub(super) static PANIC_STORING_INDEXES: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// Makes [`Transaction::write_record`] panic on this thread once it
    /// wrote (`engine::tests`).
    pub(super) static PANIC_WRITING_RECORDS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// Makes [`Transaction::recover`] panic on this thread once it
    /// finished the work (`engine::tests`).
    pub(super) static PANIC_RECOVERING: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// A file's lock table as an operation uses it ([`super::run`]). It takes
/// the table's mutex when the operation first uses it, after the store the
/// engine took (the lock order of [`super`]), and holds it until the engine
/// keeps or takes back the operation's edits.
///
/// Locks taken ([`Self::lock`]) are released again when the operation's
/// other effects cannot be stored; releases of what a commit wrote
/// ([`Self::release_once_stored`]) wait until they are. The other edits
/// are made at once and kept: they are those of operations that store
/// nothing but lock-table edits, which are answered before they are
/// durable (docs/server-storage.md 5). An authoring edit's locks
/// ([`Self::lock_while_stored`]) are held while its changes are stored,
/// without the table's mutex, and released after, stored or not.
pub struct Locks<'a> {
    table: &'a Mutex<LockTable>,
    held: Option<MutexGuard<'a, LockTable>>,
    /// Locks taken, in order.
    taken: Vec<(KeyPath, LockUser, u16)>,
    /// Paths whose locks a user releases once the operation is stored.
    releases: Vec<(KeyPath, LockUser)>,
    /// Locks held while the operation's changes are stored, released
    /// after ([`Self::lock_while_stored`]).
    while_stored: Vec<(KeyPath, LockUser, u16)>,
}

impl<'a> Locks<'a> {
    pub(super) fn new(table: &'a Mutex<LockTable>) -> Self {
        Locks {
            table,
            held: None,
            taken: Vec::new(),
            releases: Vec::new(),
            while_stored: Vec::new(),
        }
    }

    /// The table, taken now if it is not held yet; also after a request
    /// panicked holding it (`Hosted::lock_table`).
    fn table(&mut self) -> &mut LockTable {
        let table = self.table;
        self.held
            .get_or_insert_with(|| table.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// `LockTable::holds`.
    pub fn holds(&mut self, path: &[Vec<u8>], user: LockUser, lock_type: u16) -> bool {
        self.table().holds(path, user, lock_type)
    }

    /// `LockTable::held_by_other`.
    pub fn held_by_other(&mut self, path: &[Vec<u8>], user: LockUser) -> bool {
        self.table().held_by_other(path, user)
    }

    /// `LockTable::lock`; released again if the operation is not stored.
    pub fn lock(
        &mut self,
        path: &[Vec<u8>],
        user: LockUser,
        lock_type: u16,
        holder: &str,
    ) -> Result<(), Conflict> {
        self.table().lock(path, user, lock_type, holder)?;
        self.taken.push((path.to_vec(), user, lock_type));
        Ok(())
    }

    /// `LockTable::upgrade`, kept.
    pub fn upgrade(
        &mut self,
        path: &[Vec<u8>],
        user: LockUser,
        from: u16,
        to: u16,
        holder: &str,
    ) -> Result<(), Conflict> {
        self.table().upgrade(path, user, from, to, holder)
    }

    /// `LockTable::unlock`, kept.
    pub fn unlock(&mut self, path: &[Vec<u8>], user: LockUser, lock_type: u16) -> bool {
        self.table().unlock(path, user, lock_type)
    }

    /// Take the table now, before the operation reads anything else: a
    /// server script's commit held it from its start before the engine
    /// interface, so no lock request ran between its checks and its store.
    pub fn hold(&mut self) {
        self.table();
    }

    /// `LockTable::release_path`, kept.
    pub fn release_path(&mut self, path: &[Vec<u8>], user: LockUser) {
        self.table().release_path(path, user);
    }

    /// `LockTable::release_txn`, kept.
    pub fn release_txn(&mut self, user: LockUser) {
        self.table().release_txn(user);
    }

    /// `LockTable::release_connection`, kept.
    pub fn release_connection(&mut self, connection: u32) {
        self.table().release_connection(connection);
    }

    /// Take `locks` (path, type) in order for `user`, or none of them, and
    /// let go of the table again: the holders of the first that conflicts.
    /// They keep other users from what the operation changes while it is
    /// stored, and are released after the engine lets go of the store,
    /// whether it was stored or not ([`Self::after_stored`]): an authoring
    /// edit's locks, which op240 has always held this way, the table's
    /// mutex taken only while it takes and releases them. An operation that
    /// takes these uses the table for nothing else.
    pub fn lock_while_stored(
        &mut self,
        user: LockUser,
        holder: &str,
        locks: &[(KeyPath, u16)],
    ) -> Result<(), String> {
        let table = self.table();
        let mut refused = None;
        for (n, (path, lock_type)) in locks.iter().enumerate() {
            if let Err(conflict) = table.lock(path, user, *lock_type, holder) {
                for (path, lock_type) in &locks[..n] {
                    table.unlock(path, user, *lock_type);
                }
                refused = Some(conflict.users);
                break;
            }
        }
        if refused.is_none() {
            let held = locks.iter().map(|(path, t)| (path.clone(), user, *t));
            self.while_stored.extend(held);
        }
        self.held = None;
        refused.map_or(Ok(()), Err)
    }

    /// Release the locks [`Self::lock_while_stored`] took, now: the
    /// operation was refused after it took them.
    pub fn release_while_stored(&mut self) {
        let held = std::mem::take(&mut self.while_stored);
        let table = self.table();
        for (path, user, lock_type) in &held {
            table.unlock(path, *user, *lock_type);
        }
        self.held = None;
    }

    /// The locks held while the operation was stored, which the engine
    /// releases once it lets go of the store ([`release`]).
    pub(super) fn after_stored(&mut self) -> Vec<(KeyPath, LockUser, u16)> {
        std::mem::take(&mut self.while_stored)
    }

    /// `LockTable::release_path` once the operation is stored: a commit
    /// releases the records it wrote, and keeps them locked when its save
    /// fails.
    pub fn release_once_stored(&mut self, path: KeyPath, user: LockUser) {
        self.releases.push((path, user));
    }

    /// The operation was stored: make the releases that waited for it.
    pub(super) fn keep(mut self) {
        let releases = std::mem::take(&mut self.releases);
        if releases.is_empty() {
            return;
        }
        let table = self.table();
        for (path, user) in releases {
            table.release_path(&path, user);
        }
    }

    /// The operation was not stored: release the locks it took, newest
    /// first, which leaves the table as it was before them
    /// (`LockTable::unlock` undoes `LockTable::lock` exactly), and drop
    /// the releases that waited.
    pub(super) fn take_back(mut self) {
        let taken = std::mem::take(&mut self.taken);
        if taken.is_empty() {
            return;
        }
        let table = self.table();
        for (path, user, lock_type) in taken.into_iter().rev() {
            table.unlock(&path, user, lock_type);
        }
    }
}

/// Release `locks` of `table` ([`Locks::after_stored`]), taking the table's
/// mutex for it.
pub(super) fn release(table: &Mutex<LockTable>, locks: Vec<(KeyPath, LockUser, u16)>) {
    if locks.is_empty() {
        return;
    }
    let mut table = table.lock().unwrap_or_else(PoisonError::into_inner);
    for (path, user, lock_type) in &locks {
        table.unlock(path, *user, *lock_type);
    }
}
