//! What an operation runs against ([`super::run`]): the file's tree as the
//! engine holds it ([`Hold`], [`Transaction`]) and its lock table
//! ([`Locks`]). Both keep what the engine needs to take the operation's
//! effects back when they cannot be stored.

use std::sync::{Mutex, MutexGuard, PoisonError};

use om_data::rules::RecordEnvironment;
use om_hbam::OwnedNode;
use om_protocol::write::{Conflict, KeyPath};

use crate::library::{BinaryData, Stored};
use crate::locks::{LockTable, LockUser};
use crate::store::{IndexId, Journal, MemberKey, Store};

/// What the engine holds of a file for an operation: what the operation
/// needs, and no more (`FileEngine::submit`).
pub enum Hold<'h, 'a> {
    /// Nothing of the tree: only the lock table (op61, op62, op65).
    LockTable,
    /// The tree, for reading (op60).
    View(&'h Store),
    /// A transaction on the tree (op70, op71, op72, op13, op73, op86's
    /// writes and a find's indexes, op87, op88 and op240).
    Transaction(&'h mut Transaction<'a>),
}

/// A transaction on a file's tree, over the file engine's store: what an
/// operation reads and the changes it makes, kept so that the engine can
/// store them or take them back.
///
/// It exposes the `Store` methods the operations use, and `view` for the
/// state derived from the tree (a privilege set's hidden fields and record
/// limits, the decoded catalog an authoring edit checks). The changes are
/// a commit's record writes and an authoring edit's catalog members
/// (`Journal`), op71's reservation and op72's serial value, and the schema
/// changes of op13, op73 and op86 with a find's stored indexes and op87's
/// and op88's container data, which are made in place and not taken back
/// (their saves are not checked yet).
pub struct Transaction<'a> {
    pub(super) store: &'a mut Store,
    /// The commit the record writes go into ([`Self::begin_commit`]).
    pub(super) journal: Option<Journal>,
    /// op71's reservation, to take back when it cannot be saved.
    pub(super) reservation: Option<Reservation>,
    /// Log lines that say a change was made, written once it is stored.
    pub(super) stored_lines: Vec<String>,
    /// The container libraries deleted tables took with them
    /// ([`Self::apply`]), whose thumbnails the engine forgets.
    pub(super) released_libraries: Vec<Vec<Vec<u8>>>,
    /// Fields an authoring edit added, counted as computed once the edit
    /// is stored ([`Self::add_field_basis_once_stored`]).
    pub(super) field_bases: Vec<(Vec<u8>, u64)>,
}

/// The last issued id of `table` as it was before a reservation
/// (`Store::take_back_record_ids`).
pub(super) struct Reservation {
    pub(super) table: Vec<u8>,
    pub(super) before: Option<OwnedNode>,
    pub(super) dirty: bool,
}

impl<'a> Transaction<'a> {
    pub(super) fn new(store: &'a mut Store) -> Self {
        Transaction {
            store,
            journal: None,
            reservation: None,
            stored_lines: Vec::new(),
            released_libraries: Vec::new(),
            field_bases: Vec::new(),
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

    pub fn node(&self, path: &[&[u8]]) -> Option<&OwnedNode> {
        self.store.node(path)
    }

    /// Reserve `count` record ids of `table` (`Store::reserve_record_ids`).
    /// The reservation is not part of a commit: the engine saves it, or
    /// takes it back byte for byte when the save fails.
    pub fn reserve_record_ids(&mut self, table: &[u8], count: u32) -> Option<u64> {
        let before = self.store.last_record_id_node(table);
        let dirty = self.store.has_unsaved_changes();
        let first = self.store.reserve_record_ids(table, count)?;
        self.reservation.get_or_insert(Reservation {
            table: table.to_vec(),
            before,
            dirty,
        });
        Some(first)
    }

    /// A field's next serial value, read and possibly replaced or advanced
    /// (`Store::serial_value`). Not taken back: a serial value handed out
    /// is never handed out again.
    pub fn serial_value(
        &mut self,
        table: &[u8],
        field: &[u8],
        replace: Option<&str>,
        increment: Option<i32>,
    ) -> Option<String> {
        self.store.serial_value(table, field, replace, increment)
    }

    /// Start the commit the record writes go into, which the engine saves
    /// whole or takes back (`Store::save_commit`), even if it writes
    /// nothing.
    pub fn begin_commit(&mut self) {
        if self.journal.is_none() {
            self.journal = Some(self.store.begin_commit());
        }
    }

    /// Write record `id` of `table` (`None` deletes it) within the commit
    /// (`Store::write_record_in`).
    pub fn write_record(&mut self, table: &[u8], id: u64, record: Option<OwnedNode>) {
        let journal = self
            .journal
            .get_or_insert_with(|| self.store.begin_commit());
        self.store.write_record_in(journal, table, id, record);
    }

    /// Run `work`: when it panics, the commit's changes are put back before
    /// the panic goes on, as `Store::guarded` does, so the store a poisoned
    /// lock hands to the next request is the one before the operation.
    pub fn guarded<T>(&mut self, work: impl FnOnce(&mut Self) -> T) -> T {
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(self)));
        match done {
            Ok(value) => value,
            Err(panic) => {
                if let Some(journal) = self.journal.take() {
                    self.store.roll_back(journal);
                }
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// Log `line` once the transaction is stored, and not if it is not.
    pub fn once_stored(&mut self, line: String) {
        self.stored_lines.push(line);
    }

    /// op13: put `node` at `path` (`Store::put_subtree`); whether it was
    /// stored.
    pub fn put_subtree(&mut self, path: &[Vec<u8>], node: OwnedNode) -> bool {
        self.store.put_subtree(path, node)
    }

    /// op86's UploadMultiple and DeleteTable writes (`Store::apply`). The
    /// libraries deleted tables took go with them, and so do their
    /// thumbnails ([`Self::released_libraries`]).
    pub fn apply(&mut self, writes: Vec<om_query::Write>) {
        let released = self.store.apply(writes);
        self.released_libraries.extend(released);
    }

    /// GenerateMemberKey's issued keys (`Store::write_member_keys`).
    pub fn write_member_keys(&mut self, keys: Vec<MemberKey>) {
        self.store.write_member_keys(keys);
    }

    /// Indexes finds built on demand (`Store::store_indexes`).
    pub fn store_indexes(&mut self, indexes: Vec<IndexId>) {
        self.store.store_indexes(indexes);
    }

    /// Replace catalog members the schema does not hold, such as a script
    /// and the script catalog's index, within the commit
    /// (`Store::replace_members_in`).
    pub fn replace_members(&mut self, members: Vec<(Vec<Vec<u8>>, OwnedNode)>) {
        let journal = self
            .journal
            .get_or_insert_with(|| self.store.begin_commit());
        self.store.replace_members_in(journal, members);
    }

    /// Replace the schema node at `path` within the commit, the schema read
    /// again (`Store::replace_schema_in`).
    pub fn replace_schema(&mut self, path: Vec<Vec<u8>>, node: OwnedNode) {
        let journal = self
            .journal
            .get_or_insert_with(|| self.store.begin_commit());
        self.store.replace_schema_in(journal, path, node);
    }

    /// Remove the schema node at `path` within the commit
    /// (`Store::remove_schema_in`).
    pub fn remove_schema(&mut self, path: Vec<Vec<u8>>) {
        let journal = self
            .journal
            .get_or_insert_with(|| self.store.begin_commit());
        self.store.remove_schema_in(journal, path);
    }

    /// Count field `field` of `table`, which the commit adds, as computed
    /// once the commit is stored (`Store::add_field_basis`), and not if it
    /// is not.
    pub fn add_field_basis_once_stored(&mut self, table: Vec<u8>, field: u64) {
        self.field_bases.push((table, field));
    }

    /// op88: store container data in the library at `path`
    /// (`Store::store_binary`).
    pub fn store_binary(
        &mut self,
        path: &[&[u8]],
        data: BinaryData,
        unique_id: impl FnMut() -> [u8; 16],
    ) -> Option<Stored> {
        self.store.store_binary(path, data, unique_id)
    }

    /// op87: add `delta` to a library member's reference count
    /// (`Store::adjust_reference_count`).
    pub fn adjust_reference_count(
        &mut self,
        path: &[&[u8]],
        member: &[u8],
        delta: i32,
    ) -> Option<u32> {
        self.store.adjust_reference_count(path, member, delta)
    }

    /// op73: a table's deferred field work (`Store::update_table`), each
    /// record's environment for a recalculation from `environment`, and
    /// the unique ids of container data it stores from `unique_id`.
    pub fn update_table(
        &mut self,
        table: &[u8],
        environment: &mut dyn FnMut() -> RecordEnvironment,
        unique_id: &mut dyn FnMut() -> [u8; 16],
    ) -> Result<bool, &'static str> {
        self.store.update_table(table, environment, unique_id)
    }
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
