//! Record writes (spec `docs/re/write-path.md`): creating records (op71),
//! serial numbers (op72), staging uploads (op14), committing them (op70),
//! locks (op60, op61, op62, op65; also Manage Database's,
//! `docs/re/schema-path.md` 4) and the change notifications other clients
//! receive.
//!
//! op70, op71, op72, the lock requests, a server script's commit
//! ([`commit_records`]) and its Set Next Serial Value are operations of the
//! engine interface ([`crate::engine`]): the functions here are their work,
//! which [`crate::engine::run`] calls against a transaction and the lock
//! table, and the engine saves. Staging (op14) works on the session's
//! temporary area, not on the file.
//!
//! Locks on shared state are always taken in one order: the file's store,
//! then its lock table, then its registrations, then the mailboxes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use log::{info, warn};
use om_hbam::slot::uint_segment;
use om_hbam::tree::{memory, read_owned};
use om_hbam::{serial, OwnedNode, SlotReader};
use om_protocol::write::{
    attention, record_message, Conflict, CreateRecordReply, GetSerial, GetSerialReply, KeyPath,
    LockReply, Notification, COMMIT_DELETED, COMMIT_NEW, LOCK_CONFLICT, LOCK_WRITE,
    SERIAL_INCREMENT, SERIAL_REPLACE,
};

use crate::connection::ACCESS_DENIED;
use crate::engine::{
    CommitRecords, CommitReply, CommitScriptRecords, Environment, Locks, ReleaseLock, ReleaseLocks,
    ReserveRecordIds, SetNextSerial, Staged, TakeLock, Transaction, UpgradeLock,
};
use crate::hosting::Hosted;
use crate::library::BinaryData;
use crate::locks::LockUser;
use crate::modcount;
use crate::store::Store;
use crate::uploads::Charge;

/// Key of a table's records, `[T, 05]`.
const RECORDS: &[u8] = &[0x05];

/// FileMaker errors for a missing record, table and field, and a record
/// another user holds.
const RECORD_MISSING: u32 = 101;
const TABLE_MISSING: u32 = 106;
const FIELD_MISSING: u32 = 102;
const RECORD_IN_USE: u16 = 301;

/// The most record ids one op71 may reserve (OpenMaker's choice). FileMaker
/// Pro asks for 1 or 25 (write-path.md 3.1), OpenMaker's client and server
/// scripts for 1. What FileMaker Pro asks for while it imports into a
/// hosted file is unmeasured, so the cap leaves room above 25.
const MAX_CREATE_COUNT: u32 = 1_000;

/// The most locks one connection's requests may hold, over all its files
/// (OpenMaker's choice; FileMaker Server's limit is unmeasured). A user
/// editing a record holds one, and one per unique field it checks (1.3); a
/// new record the block op71 reserved, 25 at most from FileMaker Pro;
/// Manage Database `[08]`, the catalogs it edits and a table lock per
/// table whose fields changed (schema-path.md 4); a script transaction in
/// the client one per record it changes, until it commits. A lock taken
/// again by its holder counts once. Server scripts run under lock users of
/// their own and are not held to it.
pub const MAX_LOCKS: usize = 100_000;

/// The room of a lock user no cap applies to: a server script's session
/// rather than a client's connection.
pub const UNCAPPED: usize = usize::MAX;

/// The most notifications that may wait for one connection (OpenMaker's
/// choice; FileMaker Server's queue is unmeasured). Past it, or past
/// [`MAX_QUEUED_BYTES`], the queue is combined ([`Mailboxes`]).
pub const MAX_QUEUED: usize = 50_000;

/// The most bytes the notifications waiting for one connection may take in
/// a backchannel: what one reply with the largest `maxreturn` carries
/// (network-protocol.md 5.6.2), so a client that asks for that much takes
/// the whole queue at once.
pub const MAX_QUEUED_BYTES: usize = 0x20_0000;

/// The status for a refused request whose code FileMaker does not define
/// (FileMaker error 3, "command is unavailable"; OpenMaker's choice).
pub(crate) const UNAVAILABLE: u32 = 3;
/// [`UNAVAILABLE`] as a lock reply's status, which is a u16.
pub(crate) const LOCK_UNAVAILABLE: u16 = 3;

/// One applied change, to notify other clients of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// `[T, 05, R]`.
    pub path: KeyPath,
    /// The attention code: add, modify or delete, of a record.
    pub code: u32,
}

/// The notifications waiting for each connection, delivered in the
/// backchannel of its Perform replies in the order they were queued
/// (write-path.md 6.4), as much of the queue as each reply's `maxreturn`
/// leaves room for.
///
/// A queue that grows past [`MAX_QUEUED`] notifications or
/// [`MAX_QUEUED_BYTES`] (a client that has not sent a Perform for a while,
/// FileMaker Pro tickling while its user is away, or a bulk change by a
/// script) is combined, losing nothing a client needs to read the file
/// again: of notifications alike only the latest is kept, and those of
/// records `[T, 05, R]` become one of the table's records `[T, 05]` with
/// the same code, the path FileMaker Pro itself gives other clients when
/// Manage Database changed a table's fields (schema-path.md 9). When that
/// does not halve the queue, the connection is closed
/// ([`Mailboxes::overflowed`]): its client cannot miss that, and reads the
/// file again when it reconnects. These are OpenMaker's choices.
#[derive(Debug, Default)]
pub struct Mailboxes(Mutex<HashMap<u32, Mailbox>>);

/// One connection's queue.
#[derive(Debug, Default)]
pub(crate) struct Mailbox {
    queue: VecDeque<Queued>,
    /// What the queue takes in a backchannel ([`Queued::size`]).
    bytes: usize,
    /// Still past half its bounds once combined: the connection is to be
    /// closed, and nothing more is queued for it.
    overflowed: bool,
}

/// A notification waiting for a connection: the change, and how the
/// connection's registration on `file` addresses it (write-path.md 6.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Queued {
    file: u32,
    address: u32,
    client_file: i32,
    code: u32,
    path: KeyPath,
}

impl Queued {
    /// The marshalled message (write-path.md 6.3).
    fn notification(&self) -> Notification {
        Notification {
            code: self.code,
            message: record_message(self.address, self.code, self.client_file, &self.path),
        }
    }

    /// Its bytes in a backchannel: the code and length (6), the message
    /// header (16), the client file id item (4 + 2) and the path item (its
    /// length byte, the segments with theirs, and 2).
    fn size(&self) -> usize {
        6 + 16 + 6 + 1 + self.path.iter().map(|s| 1 + s.len()).sum::<usize>() + 2
    }

    /// A record's notification as one of its table's records.
    fn widen(&mut self) {
        let record = self.code & 0xff == attention::RECORD;
        if record && self.path.len() == 3 && self.path[1] == RECORDS {
            self.path.truncate(2);
        }
    }
}

impl Mailbox {
    fn over(&self, entries: usize, bytes: usize) -> bool {
        self.queue.len() > entries || self.bytes > bytes
    }

    /// Combine the queue ([`Mailboxes`]), keeping the order of what is
    /// kept.
    fn combine(&mut self) {
        let mut queued: Vec<Queued> = std::mem::take(&mut self.queue).into();
        queued.iter_mut().for_each(Queued::widen);
        let latest: Vec<bool> = {
            let mut seen = HashSet::new();
            let mut latest: Vec<bool> = queued.iter().rev().map(|q| seen.insert(q)).collect();
            latest.reverse();
            latest
        };
        self.queue = queued
            .into_iter()
            .zip(latest)
            .filter_map(|(q, latest)| latest.then_some(q))
            .collect();
        self.bytes = self.queue.iter().map(Queued::size).sum();
    }
}

impl Mailboxes {
    /// Every connection's mailbox, also after a thread panicked holding
    /// them: each change is one push, pop, retain, remove or a combining
    /// that replaces the queue whole, which leaves them whole, and a
    /// session's end must still close its own.
    pub(crate) fn boxes(&self) -> MutexGuard<'_, HashMap<u32, Mailbox>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn post(&self, connection: u32, queued: Queued) {
        let mut boxes = self.boxes();
        let mailbox = boxes.entry(connection).or_default();
        if mailbox.overflowed {
            return;
        }
        mailbox.bytes += queued.size();
        mailbox.queue.push_back(queued);
        if !mailbox.over(MAX_QUEUED, MAX_QUEUED_BYTES) {
            return;
        }
        let before = mailbox.queue.len();
        mailbox.combine();
        if mailbox.over(MAX_QUEUED / 2, MAX_QUEUED_BYTES / 2) {
            warn!(
                "connection 0x{connection:08x}: {} notifications ({} bytes) wait for it after \
                 combining {before}; closing it so that its client reads the files again",
                mailbox.queue.len(),
                mailbox.bytes
            );
            *mailbox = Mailbox {
                overflowed: true,
                ..Mailbox::default()
            };
        } else {
            info!(
                "connection 0x{connection:08x}: combined {before} waiting notifications into {}",
                mailbox.queue.len()
            );
        }
    }

    /// The notifications waiting for `connection` that fit, from the
    /// first, in a backchannel of at most `room` bytes; the rest wait for
    /// the next reply.
    pub fn take(&self, connection: u32, room: usize) -> Vec<Notification> {
        let mut boxes = self.boxes();
        let Some(mailbox) = boxes.get_mut(&connection) else {
            return Vec::new();
        };
        // The backchannel's `ff 00`.
        let mut used = 2;
        let mut taken = Vec::new();
        while let Some(size) = mailbox.queue.front().map(Queued::size) {
            if used + size > room {
                break;
            }
            let Some(queued) = mailbox.queue.pop_front() else {
                break;
            };
            used += size;
            mailbox.bytes = mailbox.bytes.saturating_sub(size);
            let notification = queued.notification();
            debug_assert_eq!(6 + notification.message.len(), size);
            taken.push(notification);
        }
        taken
    }

    /// Whether the notifications waiting for `connection` grew past what
    /// combining them can bound: the connection is then to be closed.
    pub fn overflowed(&self, connection: u32) -> bool {
        self.boxes().get(&connection).is_some_and(|m| m.overflowed)
    }

    /// Discard notifications for one file while its registration is locked.
    pub fn clear_file(&self, connection: u32, file: u32) {
        if let Some(mailbox) = self.boxes().get_mut(&connection) {
            mailbox.queue.retain(|q| q.file != file);
            mailbox.bytes = mailbox.queue.iter().map(Queued::size).sum();
        }
    }

    /// Forget a connection that ended.
    pub fn close(&self, connection: u32) {
        self.boxes().remove(&connection);
    }
}

/// A one-segment key's segment.
fn single(key: &KeyPath) -> Option<&[u8]> {
    match key.as_slice() {
        [segment] => Some(segment),
        _ => None,
    }
}

/// The reply of an op71 that reserved nothing.
pub(crate) fn refused_ids(status: u32) -> CreateRecordReply {
    CreateRecordReply {
        conflict: None,
        status,
        record: vec![vec![0x00]],
    }
}

/// op71: reserve `count` record ids after the table's last issued id and
/// lock them for the transaction (write-path.md 3.1). `room` is how many
/// more locks the lock user's connection may hold ([`MAX_LOCKS`]): more ids
/// than that are refused as a lock conflict is, before any is reserved.
/// A client's op71 is `durable`: the engine saves the new last issued id
/// before the reply hands the ids out (#927), as a client keeps a new
/// record's id across a server restart and commits it later, and op70's
/// reply cannot report a record it leaves out (write-path.md 3.4), so an id
/// it was given must be one the file counts as issued. A reservation the
/// file cannot save is taken back, byte for byte, with the locks it took,
/// and refused with the save's error ([`crate::hosting::save_error`]): no
/// id is issued that a restart could hand out again. The server's own
/// scripts and imports reserve without it: they end with the server, so no
/// id of theirs outlives a restart, and their commit saves the file once
/// rather than once per record; the script's end saves the ids of records
/// it did not commit (`engine::Operation::SaveFile`).
pub(crate) fn create_record(
    op: ReserveRecordIds,
    file: &str,
    tx: &mut Transaction<'_>,
    locks: &mut Locks<'_>,
) -> CreateRecordReply {
    let ReserveRecordIds {
        user,
        holder,
        request: req,
        room,
        durable: _,
    } = op;
    if tx.is_read_only() {
        return refused_ids(u32::from(ACCESS_DENIED));
    }
    let Some(table) = single(&req.table).filter(|t| tx.has_table(t)) else {
        return refused_ids(TABLE_MISSING);
    };
    // Each refusal comes before anything changes, in this order: the count,
    // the connection's lock room, then the reservation's own (a count of 0,
    // ids past the largest, a table node the last issued id cannot go
    // beneath; `Store::reserve_record_ids`), then (`durable`) the engine's
    // save of the new last issued id, which a refusal takes back. Only a
    // lock another user holds on a reserved id refuses after the ids are
    // reserved, as FileMaker Server does (write-path.md 3.1, step 4).
    if req.count == 0 || req.count > MAX_CREATE_COUNT {
        return refused_ids(UNAVAILABLE);
    }
    if usize::try_from(req.count).map_or(true, |count| count > room) {
        warn!(
            "{file:?}: {holder} may not lock {} more record id(s): its connection holds \
             {MAX_LOCKS} locks at most",
            req.count
        );
        return refused_ids(u32::from(LOCK_CONFLICT));
    }
    let Some(first) = tx.reserve_record_ids(table, req.count) else {
        return refused_ids(UNAVAILABLE);
    };
    // Inclusive: the last id may be the largest a serial key holds.
    for id in first..=first - 1 + u64::from(req.count) {
        let path = vec![table.to_vec(), RECORDS.to_vec(), serial::encode(id)];
        if locks.lock(&path, user, LOCK_WRITE, &holder).is_err() {
            return refused_ids(u32::from(LOCK_CONFLICT));
        }
    }
    tx.once_stored(format!(
        "{file:?}: reserved {} record id(s) from {first} in table {table:02x?}",
        req.count
    ));
    CreateRecordReply {
        conflict: None,
        status: 0,
        record: vec![serial::encode(first)],
    }
}

/// op72: read a field's next serial value, replacing or advancing it as the
/// flags ask (write-path.md 3.2).
pub(crate) fn get_serial(req: &GetSerial, tx: &mut Transaction<'_>) -> GetSerialReply {
    if tx.is_read_only() {
        return GetSerialReply {
            conflict: None,
            status: u32::from(ACCESS_DENIED),
            value: String::new(),
        };
    }
    let value = match (single(&req.table), single(&req.field)) {
        (Some(table), Some(field)) => tx.serial_value(
            table,
            field,
            (req.flags & SERIAL_REPLACE != 0).then_some(req.value.as_str()),
            (req.flags & SERIAL_INCREMENT != 0).then_some(req.increment),
        ),
        _ => None,
    };
    match value {
        Some(value) => GetSerialReply {
            conflict: None,
            status: 0,
            value,
        },
        None => GetSerialReply {
            conflict: None,
            status: FIELD_MISSING,
            value: String::new(),
        },
    }
}

/// A server script's Set Next Serial Value
/// ([`crate::engine::SetNextSerial`]): replace a field's next serial value
/// within a commit, which the engine saves before it counts and takes back
/// when it cannot, as a record commit. 102 for a table without the field,
/// which changes nothing.
pub(crate) fn set_next_serial(op: SetNextSerial, tx: &mut Transaction<'_>) -> Result<(), u32> {
    let SetNextSerial {
        table,
        field,
        value,
    } = op;
    tx.set_serial(&table, &field, &value)
        .map(|_| ())
        .ok_or(FIELD_MISSING)
}

/// One file's temporary area on a connection (write-path.md 3.3): the
/// records op14 uploaded that op70 has not committed, and the memory they
/// take, charged to the server's budget for clients' upload trees
/// ([`crate::uploads`]). What records leave the area gives back, and so
/// does dropping the area, however its session ends.
pub struct TempArea {
    root: OwnedNode,
    /// [`memory`] of the nodes below `root`.
    held: Charge,
}

impl TempArea {
    /// An empty area, its records charged to `held` (a charge of nothing).
    pub fn new(held: Charge) -> Self {
        Self {
            root: OwnedNode::default(),
            held,
        }
    }

    /// The memory the staged records take, as [`memory`] counts it.
    pub fn held(&self) -> usize {
        self.held.bytes()
    }

    /// Take out the records op70's commit `list` names (`[T, 05, R]`) for
    /// its operation to own ([`crate::engine::CommitRecords`]). They stay
    /// charged to the area until [`TempArea::settle`] gives back what the
    /// commit took.
    pub fn lend(&mut self, list: &OwnedNode) -> (Staged, Lent) {
        let mut staged = Staged::new();
        let mut lent = Vec::new();
        for table in &list.children {
            let Some(records) = table.child(RECORDS) else {
                continue;
            };
            for entry in &records.children {
                let path = vec![
                    table.segment.clone(),
                    RECORDS.to_vec(),
                    entry.segment.clone(),
                ];
                if staged.contains_key(&path) {
                    continue;
                }
                let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
                if let Some(record) = self.root.remove(&segments) {
                    // About, not exactly: records staged inside another one
                    // grow its child list, which counting it then includes.
                    lent.push((path.clone(), memory(&record)));
                    staged.insert(path, record);
                }
            }
        }
        (staged, Lent(lent))
    }

    /// After the commit [`TempArea::lend`] lent to: the records it did not
    /// take (`unused`) go back where they were, and what those it took held
    /// is given back.
    pub fn settle(&mut self, lent: Lent, mut unused: Staged) {
        let mut held = self.held.bytes();
        for (path, size) in lent.0 {
            match unused.remove(&path) {
                Some(record) => {
                    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
                    self.root.put(&segments, record);
                }
                None => held = held.saturating_sub(size),
            }
        }
        self.held.shrink_to(held);
    }
}

/// The records [`TempArea::lend`] lent, with the memory each held.
#[derive(Debug)]
pub struct Lent(Vec<(KeyPath, usize)>);

/// The memory of the nodes `OwnedNode::put` creates for `path` below
/// `root`: those it does not have yet.
fn missing(root: &OwnedNode, path: &[&[u8]]) -> usize {
    let mut node = Some(root);
    path.iter()
        .map(|segment| {
            node = node.and_then(|n| n.child(segment));
            match node {
                Some(_) => 0,
                None => memory(&OwnedNode::list(segment.to_vec(), Vec::new())),
            }
        })
        .sum()
}

/// Why an uploaded list was not read or staged.
#[derive(Debug)]
pub enum StageError {
    /// The upload is not a slot list.
    Malformed(String),
    /// Reading or staging it would take more memory than was reserved for
    /// it, or than the client may still hold ([`crate::uploads`]).
    Full,
}

/// Read an uploaded slot list (op14's records, an op70 commit list, an
/// op13 subtree) into a tree, unless the tree would take more than `limit`
/// bytes of memory: what was reserved for it ([`crate::uploads`]). A list
/// of short keys takes tens of times its size as a tree, so it is refused
/// as it is read rather than after (`om_hbam::tree::read_owned`).
pub fn read_upload(payload: &[u8], limit: usize) -> Result<OwnedNode, StageError> {
    // FileMaker Pro sends op14's records closed by the destination's `40`,
    // without the `80` that opens it; the slot reader skips an `80` anyway.
    read_owned(SlotReader::new(payload), limit).map_err(|e| match e {
        om_hbam::Error::TooLarge { .. } => StageError::Full,
        e => StageError::Malformed(e.to_string()),
    })
}

/// op14: unpack an uploaded slot list into a file's temporary area under
/// `dest` (write-path.md 3.3), reading it within what `reading` reserved
/// and moving what the staged records take from it into the area's charge.
/// When it does not fit, the area is left as it was. Returns how many
/// records it held.
pub fn stage_upload(
    area: &mut TempArea,
    dest: &KeyPath,
    payload: &[u8],
    reading: &mut Charge,
) -> Result<usize, StageError> {
    let records = match read_upload(payload, reading.bytes()) {
        Ok(tree) => tree.children,
        Err(StageError::Malformed(e)) => {
            return Err(StageError::Malformed(format!(
                "upload is not a slot list: {e}"
            )))
        }
        Err(full) => return Err(full),
    };
    let parents: Vec<&[u8]> = dest.iter().map(Vec::as_slice).collect();
    // The destination's nodes the area lacks, and the records, less those
    // they replace.
    let created = missing(&area.root, &parents);
    let existing = area.root.at(&parents);
    let (added, replaced) = records
        .iter()
        .fold((created, 0), |(added, replaced), record| {
            let old = existing.and_then(|parent| parent.child(&record.segment));
            (added + memory(record), replaced + old.map_or(0, memory))
        });
    // The destination's new nodes are not in the tree that was read: what
    // they add beyond the reservation is reserved now.
    let needed = added.saturating_sub(replaced);
    if needed > reading.bytes() && !reading.grow(needed - reading.bytes()) {
        return Err(StageError::Full);
    }
    let before = area.held();
    let mut held = before + created;
    let count = records.len();
    for record in records {
        let segment = record.segment.clone();
        let mut path = parents.clone();
        path.push(&segment);
        held += memory(&record);
        if let Some(old) = area.root.remove(&path) {
            held = held.saturating_sub(memory(&old));
        }
        area.root.put(&path, record);
    }
    // At most `needed`: a record the upload holds twice replaces itself.
    if held > before {
        reading.move_into(&mut area.held, held - before);
    } else {
        area.held.shrink_to(held);
    }
    Ok(count)
}

/// op70: commit the listed records the session staged (`staged`, lent by
/// its temporary area) into the file (write-path.md 3.4, 5). A record
/// another lock user holds is left alone, rather than half-applied as
/// FileMaker Server does, and so is one whose modification count is not a
/// count (5.4, [`modcount::counted`]): the uploaded record is dropped and
/// the lock kept. A new record is left alone when its id was not issued or a
/// record already has it, so it never replaces one (write-path.md 2;
/// OpenMaker's choice). The commit counts once the engine saved it: when it
/// cannot be, every record is put back, the records stay locked and the
/// save's error is returned as [`CommitError::NotSaved`], which fails the
/// op70. `account` and `access` are the account the session logged in as
/// and its privilege set then. Returns the reply, with the staged records
/// the commit did not take, and the changes applied.
pub(crate) fn commit(
    op: CommitRecords,
    file: &str,
    tx: &mut Transaction<'_>,
    locks: &mut Locks<'_>,
    environment: &dyn Environment,
) -> (CommitReply, Vec<Change>) {
    let CommitRecords {
        user,
        account,
        access,
        list,
        mut staged,
        withheld,
    } = op;
    let mut record_environment = || crate::recalc::record_environment_in(&account, environment);
    if let Some((table, action)) = refused_action(
        tx.view(),
        &staged,
        &list,
        &access,
        &mut record_environment,
        &withheld,
    ) {
        let refused = CommitError::Refused(format!(
            "commit refused: the account's privilege set does not allow {action:?} of a record of \
             table {table:02x?}"
        ));
        return (
            CommitReply {
                result: Err(refused),
                unused: staged,
            },
            Vec::new(),
        );
    }
    // A field without access keeps its stored value whatever an edit
    // uploads for it: the client never had it (`views::keep_hidden`).
    let hidden = crate::views::hidden_fields(tx.view(), &access);
    tx.begin_commit();
    let changes = tx.guarded(|tx| commit_list(file, user, locks, &mut staged, &list, tx, &hidden));
    for change in &changes {
        locks.release_once_stored(change.path.clone(), user);
        tx.once_stored(change_line(file, "", change));
    }
    (
        CommitReply {
            result: Ok(()),
            unused: staged,
        },
        changes,
    )
}

/// Why an op70 [`commit`] wrote nothing, each with its log line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitError {
    /// The account's privilege set refuses a record's action. Nothing was
    /// written.
    Refused(String),
    /// The file could not be saved: the commit was taken back and its
    /// records stay locked; or the save could not be confirmed on disk,
    /// and the file, which keeps the commit, takes no more changes.
    NotSaved(String),
}

/// The first record of commit `list` whose action the account's set
/// `access` refuses, with its table: decided by what `store` holds under
/// the write lock, as [`commit_list`] writes it. A record that does not
/// exist is created, whatever its marker says; a deletion of one that does
/// not exist writes nothing. An action limited by a formula is decided by
/// the formula on the record: the stored one for an edit or a deletion,
/// the uploaded one in `staged` for a new record (which state FileMaker
/// evaluates is unmeasured); a formula that cannot be built or evaluated
/// refuses. An edit of a stored record its view formula is not true on is
/// refused too, as the client never had that record's values. The
/// formulas read the account's `environment`, made the first time a record
/// is checked.
fn refused_action(
    store: &Store,
    staged: &Staged,
    list: &OwnedNode,
    access: &om_data::host::security::SetAccess,
    environment: &mut dyn FnMut() -> om_data::rules::RecordEnvironment,
    withheld: &crate::views::Withheld,
) -> Option<(Vec<u8>, om_data::host::security::RecordAction)> {
    use om_data::host::security::RecordAction;
    let mut made = None;
    for table in &list.children {
        let t = table.segment.as_slice();
        let Some(records) = table.child(RECORDS) else {
            continue;
        };
        for entry in &records.children {
            let exists = serial::decode(&entry.segment).is_some_and(|id| store.has_record(t, id));
            let action = if !entry.push && entry.value.as_deref() == Some(&[COMMIT_DELETED]) {
                if !exists {
                    continue;
                }
                RecordAction::Delete
            } else if exists {
                RecordAction::Edit
            } else {
                RecordAction::Create
            };
            let record_environment = made.get_or_insert_with(&mut *environment);
            // An edit of a record the account may not view, or that this
            // session was ever served without its values, whatever its edit
            // access: the client may hold that record without its values
            // (`views::ViewFilter`), so its upload, which replaces the
            // stored record, would erase them (#670). Being viewable now is
            // not enough: a formula may have turned true since the download.
            if action == RecordAction::Edit && withheld.contains(t, &entry.segment) {
                return Some((t.to_vec(), action));
            }
            if action == RecordAction::Edit {
                use om_data::host::security::RecordView;
                let path: [&[u8]; 3] = [t, RECORDS, &entry.segment];
                let viewable = match access.record_view(t) {
                    RecordView::All => true,
                    RecordView::Never => false,
                    RecordView::Where(code) => store.node(&path).is_some_and(|record| {
                        store.record_limit(t, code).is_ok_and(|limit| {
                            limit.allows(&crate::recalc::texts(record), record_environment)
                        })
                    }),
                };
                if !viewable {
                    return Some((t.to_vec(), action));
                }
            }
            if access.may(t, action) {
                continue;
            }
            let Some(code) = access.limit(t, action) else {
                return Some((t.to_vec(), action));
            };
            let path: [&[u8]; 3] = [t, RECORDS, &entry.segment];
            let record = match action {
                RecordAction::Create => {
                    staged.get(&vec![t.to_vec(), RECORDS.to_vec(), entry.segment.clone()])
                }
                _ => store.node(&path),
            };
            let allowed = record.is_some_and(|record| {
                store.record_limit(t, code).is_ok_and(|limit| {
                    limit.allows(&crate::recalc::texts(record), record_environment)
                })
            });
            if !allowed {
                return Some((t.to_vec(), action));
            }
        }
    }
    None
}

/// op70's writes of the records `list` names (see [`commit`]), taking them
/// out of `staged`; an edited record keeps its stored values of the
/// `hidden` fields.
fn commit_list(
    file: &str,
    user: LockUser,
    locks: &mut Locks<'_>,
    staged: &mut Staged,
    list: &OwnedNode,
    tx: &mut Transaction<'_>,
    hidden: &crate::views::HiddenFields,
) -> Vec<Change> {
    let mut changes = Vec::new();
    for table in &list.children {
        let t = table.segment.as_slice();
        let Some(records) = table.child(RECORDS) else {
            continue;
        };
        for entry in &records.children {
            let path = vec![t.to_vec(), RECORDS.to_vec(), entry.segment.clone()];
            let Some(id) = serial::decode(&entry.segment).filter(|_| tx.has_table(t)) else {
                warn!("{file:?}: commit of unknown record {path:02x?}");
                continue;
            };
            if locks.held_by_other(&path, user) {
                warn!("{file:?}: commit of record {id} refused: another session holds it");
                continue;
            }
            let value = entry.value.as_deref();
            let code = if !entry.push && value == Some(&[COMMIT_DELETED]) {
                if !tx.has_record(t, id) {
                    continue;
                }
                tx.write_record(t, id, None);
                attention::DELETE
            } else {
                let new = !entry.push && value == Some(&[COMMIT_NEW]);
                if new && id > tx.last_record_id(t) {
                    warn!("{file:?}: commit of new record {id} beyond the last issued id refused");
                    continue;
                }
                if new && tx.has_record(t, id) {
                    warn!("{file:?}: commit of new record {id} refused: a record has that id");
                    continue;
                }
                let Some(mut record) = staged.remove(&path) else {
                    warn!("{file:?}: commit of record {id} that was never uploaded");
                    continue;
                };
                if !modcount::counted(&record) {
                    warn!(
                        "{file:?}: commit of record {id} refused: its modification count is not \
                         one"
                    );
                    continue;
                }
                let existed = tx.has_record(t, id);
                let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
                if let (Some(fields), Some(stored)) = (hidden.get(t), tx.node(&segments)) {
                    let stored = stored.clone();
                    crate::views::keep_hidden(&mut record, &stored, fields);
                }
                tx.write_record(t, id, Some(record));
                if existed {
                    attention::MODIFY
                } else {
                    attention::ADD
                }
            };
            changes.push(Change {
                path,
                code: code | attention::RECORD,
            });
        }
    }
    changes
}

/// The log line of a committed change of a record of `file` (`by` names a
/// script's).
fn change_line(file: &str, by: &str, change: &Change) -> String {
    let done = match change.code & !attention::RECORD {
        attention::ADD => "created",
        attention::MODIFY => "modified",
        _ => "deleted",
    };
    let id = change
        .path
        .get(2)
        .and_then(|r| serial::decode(r))
        .unwrap_or(0);
    let table = change.path.first().map_or(&[][..], Vec::as_slice);
    format!("{file:?}: {by}{done} record {id} of table {table:02x?}")
}

/// One record of a server script's commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordWrite {
    pub table: u64,
    pub id: u64,
    /// The whole record as it will be stored; `None` deletes it.
    pub record: Option<OwnedNode>,
    /// Created since the file last saw it (its id reserved by op71).
    pub new: bool,
    /// Container data the script set, by cell: stored in the table's
    /// library with the commit, the cell then naming its member
    /// (containers.md 1.4, 3.4).
    pub uploads: Vec<(u64, BinaryData)>,
}

/// A server script's commit ([`crate::engine::CommitScriptRecords`]), as
/// one commit (write-path.md 3.4, 5) under one hold of the store and of the
/// lock table, taken from its start: every record is checked first (its
/// table exists, no other lock user holds it, a new record's id was issued
/// and is free, a changed or deleted record still exists, its modification
/// count is a count, [`modcount::counted`]) and nothing is written unless
/// all pass. The engine saves the commit (`Hosted::save_change`); when it
/// cannot, the records are put back and the commit fails with the save's
/// FileMaker error ([`crate::hosting::save_error`]), the records still
/// locked. Otherwise their locks are released, as op70 releases them. A
/// new container member's unique id comes from `environment`. Returns the
/// result, a FileMaker error and why, and the changes to tell the other
/// clients of.
pub(crate) fn commit_records(
    op: CommitScriptRecords,
    file: &str,
    tx: &mut Transaction<'_>,
    locks: &mut Locks<'_>,
    environment: &dyn Environment,
) -> (Result<(), (u32, String)>, Vec<Change>) {
    let CommitScriptRecords {
        user,
        records,
        bulk_delete,
    } = op;
    locks.hold();
    let path = |r: &RecordWrite| {
        vec![
            uint_segment(r.table),
            RECORDS.to_vec(),
            serial::encode(r.id),
        ]
    };
    let refused = |code: u32, reason: String| (Err((code, reason)), Vec::new());
    for record in &records {
        let path = path(record);
        let table = &path[0];
        if !tx.has_table(table) {
            return refused(TABLE_MISSING, format!("no table {table:02x?}"));
        }
        if locks.held_by_other(&path, user) {
            return refused(
                u32::from(RECORD_IN_USE),
                format!("another session holds record {}", record.id),
            );
        }
        let exists = tx.has_record(table, record.id);
        if record.new && (exists || record.id > tx.last_record_id(table)) {
            return refused(
                UNAVAILABLE,
                format!("record {} was not reserved", record.id),
            );
        }
        if !record.new && !exists {
            return refused(RECORD_MISSING, format!("record {} is gone", record.id));
        }
        if record
            .record
            .as_ref()
            .is_some_and(|r| !modcount::counted(r))
        {
            return refused(
                UNAVAILABLE,
                format!("record {}'s modification count is not one", record.id),
            );
        }
    }
    let tables: std::collections::BTreeSet<u64> = records.iter().map(|r| r.table).collect();
    // The commit is saved even when it writes nothing, as it always was.
    tx.begin_commit();
    let changes = tx.guarded(|tx| {
        let changes = write_records(tx, records, environment);
        tx.release_deleted_record_containers();
        // Delete All Records and Truncate Table that leave a table empty
        // take its library with them (`Store::drop_emptied_library_in`).
        if bulk_delete {
            for table in &tables {
                tx.drop_emptied_library(&uint_segment(*table));
            }
        }
        changes
    });
    for change in &changes {
        locks.release_once_stored(change.path.clone(), user);
        tx.once_stored(change_line(file, "script ", change));
    }
    (Ok(()), changes)
}

/// The writes of [`commit_records`], checked.
fn write_records(
    tx: &mut Transaction<'_>,
    records: Vec<RecordWrite>,
    environment: &dyn Environment,
) -> Vec<Change> {
    let path = |r: &RecordWrite| {
        vec![
            uint_segment(r.table),
            RECORDS.to_vec(),
            serial::encode(r.id),
        ]
    };
    let mut unique_id = || {
        let mut id = [0u8; 16];
        environment.fill_random(&mut id);
        id
    };
    let mut changes = Vec::with_capacity(records.len());
    for mut record in records {
        let path = path(&record);
        let code = match (&record.record, record.new) {
            (None, _) => attention::DELETE,
            (Some(_), true) => attention::ADD,
            (Some(_), false) => attention::MODIFY,
        };
        // Data the script put into container fields goes into the
        // library, as op88 stores a client's (counting the reference), and
        // the cell names its member.
        let mut counted = Vec::new();
        if let Some(node) = record.record.as_mut() {
            for (cell, data) in std::mem::take(&mut record.uploads) {
                let member = tx.store_container(&path[0], data, &mut unique_id);
                let reference = om_data::containers::reference_text(&member);
                node.set_child(om_scripts::text_node(&uint_segment(cell), &reference));
                counted.push(member);
            }
        }
        // The record's container references, counted as FileMaker Pro's
        // op87 counts a client's: a deleted record releases its values, a
        // new one takes a reference to those it copied.
        let old = tx
            .node(&[&path[0], RECORDS, &path[2]])
            .cloned()
            .filter(|_| !record.new);
        tx.count_containers(&path[0], old.as_ref(), record.record.as_ref(), &counted);
        tx.write_record(&path[0], record.id, record.record);
        changes.push(Change {
            path,
            code: code | attention::RECORD,
        });
    }
    changes
}

/// Queue a notification of each change for every other connection
/// registered on the file (write-path.md 6.2-6.3).
pub fn notify(hosted: &Hosted, mailboxes: &Mailboxes, from: u32, changes: &[Change]) {
    let registrations = hosted.registrations();
    for change in changes {
        for reg in registrations
            .iter()
            .filter(|r| r.connection != from && r.mask & change.code != 0)
        {
            mailboxes.post(
                reg.connection,
                Queued {
                    file: hosted.id,
                    address: reg.address,
                    client_file: reg.client_file,
                    code: change.code,
                    path: change.path.clone(),
                },
            );
        }
    }
}

/// The refusal of a lock that would take a connection past [`MAX_LOCKS`]:
/// a lock conflict with the requesting session itself, which the client
/// reports as it reports any other (write-path.md 4.2).
fn over_cap(file: &str, holder: &str, path: &[Vec<u8>]) -> LockReply {
    warn!(
        "{file:?}: lock of {path:02x?} refused: {holder} holds {MAX_LOCKS} locks, the most a \
         connection may"
    );
    LockReply::Refused {
        status: LOCK_CONFLICT,
        conflict: Conflict {
            self_conflict: true,
            users: holder.to_string(),
        },
    }
}

/// op60: take a lock without waiting, answering with the locked node's
/// modification count in `store` when asked (write-path.md 4.2,
/// schema-path.md 4.1). The grant is framed before the lock is taken, so a
/// lock is only held by a client the reply tells of it: a grant that cannot
/// be framed (a count longer than 255 bytes, which [`Store::mod_count`]
/// does not return) is refused with error 3 and takes nothing. A new lock
/// is refused when `room`, the locks the connection may still take
/// ([`MAX_LOCKS`]), is 0.
pub(crate) fn lock(op: TakeLock, file: &str, store: &Store, locks: &mut Locks<'_>) -> LockReply {
    let TakeLock {
        user,
        holder,
        request: req,
        room,
    } = op;
    let mod_count = req.want_mod_count.then(|| store.mod_count(&req.path));
    let granted = LockReply::Granted { mod_count };
    if let Err(e) = granted.to_result() {
        warn!(
            "{file:?}: lock of {:02x?} refused: its modification count cannot be sent: {e}",
            req.path
        );
        return LockReply::Refused {
            status: LOCK_UNAVAILABLE,
            conflict: Conflict {
                self_conflict: false,
                users: String::new(),
            },
        };
    }
    if room == 0 && !locks.holds(&req.path, user, req.lock_type) {
        return over_cap(file, &holder, &req.path);
    }
    match locks.lock(&req.path, user, req.lock_type, &holder) {
        Ok(()) => granted,
        Err(conflict) => LockReply::Refused {
            status: LOCK_CONFLICT,
            conflict,
        },
    }
}

/// op61: change the type of a lock the transaction holds, as Manage
/// Database does to replace a catalog (schema-path.md 4.2). One it does not
/// hold is taken, unless `room` is 0 ([`lock`]).
pub(crate) fn upgrade(op: UpgradeLock, file: &str, locks: &mut Locks<'_>) -> LockReply {
    let UpgradeLock {
        user,
        holder,
        request: req,
        room,
    } = op;
    let held = [req.current_type, req.new_type]
        .into_iter()
        .any(|lock_type| locks.holds(&req.path, user, lock_type));
    if room == 0 && !held {
        return over_cap(file, &holder, &req.path);
    }
    match locks.upgrade(&req.path, user, req.current_type, req.new_type, &holder) {
        Ok(()) => LockReply::Granted { mod_count: None },
        Err(conflict) => LockReply::Refused {
            status: LOCK_CONFLICT,
            conflict,
        },
    }
}

/// Whether an attention code names a change, and nothing else: its low byte
/// a scope from a record (1) to a layout object (7), its other bits only
/// add, modify and delete (write-path.md 6.2). Session events (a message,
/// closing a file, disconnecting) and the host serial, local and logout
/// flags are not changes a client may announce to the others.
fn names_a_change(code: u32) -> bool {
    let kinds = attention::ADD | attention::MODIFY | attention::DELETE;
    (1..=7).contains(&(code & 0xff)) && code & !(kinds | 0xff) == 0
}

/// op62: release one lock. A release with an attention code naming a change
/// ([`names_a_change`]) tells the other clients registered on the file,
/// which the returned change is for: Manage Database releases each catalog
/// it saved with `2` and the tables whose fields changed with `0x201`
/// (schema-path.md 9). Only a release of a lock the transaction held tells
/// anyone, so one op62 makes each other client receive at most one
/// notification, about a path the sender had locked, no longer than a key
/// (255 bytes). FileMaker Pro also sends op62 with an empty path to end a
/// notification lock, which needs nothing here.
pub(crate) fn unlock(op: ReleaseLock, locks: &mut Locks<'_>) -> Vec<Change> {
    let ReleaseLock {
        connection,
        request: req,
    } = op;
    if req.path.is_empty() {
        return Vec::new();
    }
    let user = LockUser {
        connection,
        txn: req.txn,
    };
    let released = locks.unlock(&req.path, user, req.lock_type);
    if released && names_a_change(req.attention_mask) {
        return vec![Change {
            path: req.path,
            code: req.attention_mask,
        }];
    }
    Vec::new()
}

/// op65: release a transaction's locks, or all of the connection's.
pub(crate) fn kill_all_locks(op: ReleaseLocks, locks: &mut Locks<'_>) {
    let ReleaseLocks {
        connection,
        request: req,
    } = op;
    if req.all {
        locks.release_connection(connection);
    } else {
        locks.release_txn(LockUser {
            connection,
            txn: req.txn,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #670: #137's `limited` set (`[23, 2, 5, 4]`) with its edit limit
    /// lifted, so Contacts' records are viewable only where First Name =
    /// "Allowed" and editable outright. An edit of "Denied", whose values
    /// the client never had, is refused; one of "Allowed" is not, unless
    /// the session was served "Allowed" without its values (as when the
    /// formula was not true then).
    #[test]
    #[ignore = "requires the FileMaker-made fixture of #137 (OM_CUSTOM_SETS_FIXTURE, or re/work/137-custom-sets/Contacts-custom.fmp12; not in git)"]
    fn an_edit_of_a_record_the_account_may_not_view_is_refused() {
        use om_data::host::privilege_set::Allow;
        use om_data::host::security::{set_access, RecordAction, SetAccess};
        let path = std::env::var_os("OM_CUSTOM_SETS_FIXTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../re/work/137-custom-sets/Contacts-custom.fmp12")
            });
        let file = om_hbam::HbamFile::open(&path).unwrap();
        let store = Store::load(&file, "Contacts-custom.fmp12").unwrap();
        let sets = file.subtree(&[&[23], &[2], &[5]]).unwrap().unwrap();
        let SetAccess::Custom(mut rules) = set_access(sets.child(&[4]).unwrap()).unwrap() else {
            panic!("`limited` is a custom set");
        };
        let contacts = uint_segment(0x8002);
        rules.tables.get_mut(&contacts).unwrap().actions.edit = Allow::Yes;
        let access = SetAccess::Custom(rules);
        let staged = Staged::new();
        let mut environment =
            || crate::recalc::record_environment("limited", crate::recalc::random_uuid());
        let mut refused = std::collections::BTreeMap::new();
        let mut was_withheld = crate::views::Withheld::default();
        for record in &store.node(&[&contacts, RECORDS]).unwrap().children {
            let first_name = crate::recalc::texts(record).remove(&1).unwrap_or_default();
            let entry = OwnedNode {
                segment: record.segment.clone(),
                value: Some(Vec::new()),
                ..OwnedNode::default()
            };
            let records = OwnedNode {
                segment: RECORDS.to_vec(),
                push: true,
                children: vec![entry],
                ..OwnedNode::default()
            };
            let list = OwnedNode {
                push: true,
                children: vec![OwnedNode {
                    segment: contacts.clone(),
                    push: true,
                    children: vec![records],
                    ..OwnedNode::default()
                }],
                ..OwnedNode::default()
            };
            let none = crate::views::Withheld::default();
            let action = refused_action(&store, &staged, &list, &access, &mut environment, &none);
            was_withheld.mark(&contacts, &record.segment);
            let after = refused_action(
                &store,
                &staged,
                &list,
                &access,
                &mut environment,
                &was_withheld,
            );
            refused.insert(first_name, (action.map(|(_, a)| a), after.map(|(_, a)| a)));
        }
        let edit = Some(RecordAction::Edit);
        assert_eq!(
            refused,
            std::collections::BTreeMap::from([
                ("Allowed".to_owned(), (None, edit)),
                ("Denied".to_owned(), (edit, edit)),
            ])
        );
        // A table whose records may never be viewed: every edit is refused.
        let SetAccess::Custom(mut rules) = access else {
            unreachable!()
        };
        rules.tables.get_mut(&contacts).unwrap().actions.view = Allow::No;
        let access = SetAccess::Custom(rules);
        for record in &store.node(&[&contacts, RECORDS]).unwrap().children {
            let entry = OwnedNode {
                segment: record.segment.clone(),
                value: Some(Vec::new()),
                ..OwnedNode::default()
            };
            let list = OwnedNode {
                push: true,
                children: vec![OwnedNode {
                    segment: contacts.clone(),
                    push: true,
                    children: vec![OwnedNode {
                        segment: RECORDS.to_vec(),
                        push: true,
                        children: vec![entry],
                        ..OwnedNode::default()
                    }],
                    ..OwnedNode::default()
                }],
                ..OwnedNode::default()
            };
            let none = crate::views::Withheld::default();
            let action = refused_action(&store, &staged, &list, &access, &mut environment, &none);
            assert_eq!(action.map(|(_, a)| a), edit);
        }
    }

    /// What a temporary area holds is counted as records are staged,
    /// replaced and committed, without drifting: a client that commits what
    /// it stages never nears `MAX_STAGED`. Staging beyond the room changes
    /// nothing.
    #[test]
    fn staged_records_are_counted_without_drift() {
        let dest: KeyPath = vec![vec![0x80, 0x01], vec![0x05]];
        let upload = |id: u8, value: &[u8]| {
            let record = OwnedNode::list(vec![id], vec![OwnedNode::leaf(vec![0x02], value)]);
            om_protocol::write::upload_payload(vec![record]).unwrap()
        };
        let records = |area: &TempArea| {
            area.root
                .at(&[&[0x80, 0x01], &[0x05]])
                .map_or(0, |records| {
                    records.children.iter().map(memory).sum::<usize>()
                })
        };
        let parents = memory(&OwnedNode::list(vec![0x80, 0x01], Vec::new()))
            + memory(&OwnedNode::list(vec![0x05], Vec::new()));
        let budget = crate::uploads::UploadMemory::new(usize::MAX);
        let client: std::net::IpAddr = [192, 0, 2, 1].into();
        let reserve = |bytes| budget.reserve(client, bytes).unwrap();
        let mut area = TempArea::new(Charge::none(&budget, client));
        stage_upload(&mut area, &dest, &upload(1, b"abc"), &mut reserve(1 << 20)).unwrap();
        assert_eq!(area.held(), parents + records(&area));
        // The same record again replaces it.
        stage_upload(
            &mut area,
            &dest,
            &upload(1, b"abcdef"),
            &mut reserve(1 << 20),
        )
        .unwrap();
        assert_eq!(area.held(), parents + records(&area));
        // No room for another record: refused, and the area is unchanged.
        let held = area.held();
        let refused = stage_upload(&mut area, &dest, &upload(2, b"x"), &mut reserve(16));
        assert!(matches!(refused, Err(StageError::Full)), "{refused:?}");
        assert_eq!(area.held(), held);
        assert!(area.root.at(&[&[0x80, 0x01], &[0x05], &[2]]).is_none());
        // What the readings did not use went back; the area's charge stays.
        assert_eq!(budget.in_use(), area.held());
        // A commit lends the records its list names: one it does not take
        // goes back as it was, still charged.
        let records = OwnedNode::list(vec![0x05], vec![OwnedNode::leaf(vec![1], vec![COMMIT_NEW])]);
        let list = OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::list(vec![0x80, 0x01], vec![records])],
        );
        let before = area.root.clone();
        let (staged, lent) = area.lend(&list);
        assert_eq!(staged.len(), 1);
        area.settle(lent, staged);
        assert_eq!((&area.root, area.held()), (&before, held));
        // Committing it leaves the destination's own nodes.
        let (_, lent) = area.lend(&list);
        area.settle(lent, Staged::new());
        assert_eq!(area.held(), parents);
        drop(area);
        assert_eq!(budget.in_use(), 0);
    }
}
