//! The engine interface (`docs/server-storage.md` 5) for record writes,
//! locks and schema changes: every change of this kind a request makes is
//! an [`Operation`], a value that owns its data, [`run`] against what the
//! engine holds of the file, and answered with an [`Outcome`]: the reply
//! and the notices for other clients. Effects are what the run did to the
//! [`Transaction`] and the [`Locks`]. The file engine ([`FileEngine`])
//! stores them, and only then does the caller encode the reply and deliver
//! the notices.
//!
//! Operations so far: commit records (op70), reserve record ids (op71),
//! advance a serial (op72), take, upgrade and release locks (op60, op61,
//! op62, op65), upload to a catalog (op13), store what an op86 program's
//! schema instructions changed (UploadMultiple, GenerateMemberKey,
//! DeleteTable), store the indexes a find built on demand, update a table
//! (op73), store container data and adjust a library member's reference
//! count (op88, op87), an authoring edit and the authoring extension's
//! save (op240). The other writes (accounts, a server script's commit)
//! still change the file from their handlers (#673).
//!
//! # Lock order
//!
//! [`FileEngine::submit`] takes, in this order and only what the operation
//! needs: the file's store (`Hosted::read_store` for op60,
//! `Hosted::write_store` for op70, op71, op72, the schema operations,
//! op87, op88 and op240), then its lock table (`Hosted::locks`, when the
//! operation first uses it, [`Locks`]; the schema operations, op87, op88
//! and op240's save never do). It holds both while it saves, and lets go
//! of the lock table first; dropping the store's write guard takes the
//! file's accounts while it still holds the store
//! (`hosting::StoreWriteGuard`). op61, op62 and op65 take only the lock
//! table, as before. [`FileEngine::submit_draft`] completes its draft
//! right after it takes the store, from state the write guard gives it
//! alone (see "Drafts"). An authoring edit (op240) takes the lock table
//! only while it takes its locks, and again while it releases them when
//! it is refused after taking them; it saves under the store alone, and
//! its locks are released after the engine lets go of the store, the lock
//! table taken on its own for that ([`Locks::lock_while_stored`]), as
//! op240 always held them. The one other lock taken inside `submit` is the
//! file's thumbnail cache (`Hosted::thumbnails`), under the store, when
//! op86's DeleteTable took a table's container library: store, then
//! thumbnails, as a server script's commit and op96 take them. Nothing
//! else: the caller delivers notices after it returns, taking the file's
//! registrations and then the mailboxes (`writes::notify`), and a
//! session's own state (its temporary area, the records it was served
//! withheld, the memory its uploads are charged) is read before and
//! settled after. This is the order the write path has always used
//! (store, lock table, registrations, mailboxes), with the last two no
//! longer inside the first two.
//!
//! # Drafts
//!
//! Finds and op86 programs run under the store's read lock, as reads, and
//! leave state of the running server outside the file's tree: the indexes
//! they built on demand and the member keys GenerateMemberKey issued wait
//! in the store until a write stores them. Whichever write first holds the
//! store stores everything waiting then, as before the engine interface.
//! So the request that stores them submits a [`Draft`], which names no
//! indexes or keys, and [`FileEngine::submit_draft`] completes it under the
//! store's write lock, taking what waits into the [`Operation`] it then
//! runs. [`run`] only sees completed operations, and a completed one is
//! submitted with [`FileEngine::submit`], which never completes it again.
//! A recording gets each operation by reference, completed, before it
//! runs (`FileEngine::recording`); that is what it holds and a replay
//! submits. The running server records nothing, so no operation's data is
//! copied. Member keys a draft took go back to waiting if its run is left
//! by a panic before it writes them, as they stayed waiting before when
//! storing the indexes panicked. The store engine will have to make
//! issuing a member key and building an index operations of their own.

mod file;
mod schema;
mod transaction;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use log::error;
use om_data::host::security::SetAccess;
use om_hbam::OwnedNode;
use om_protocol::authoring as design;
use om_protocol::container::{AdjustReferenceCount, UploadBinaryReply, COUNT_FAILED};
use om_protocol::schema::Upgrade;
use om_protocol::write::{
    Conflict, CreateRecord, CreateRecordReply, GetSerial, GetSerialReply, KeyPath, KillAllLocks,
    Lock, LockReply, Unlock,
};

use crate::hosting::Hosted;
use crate::library::BinaryData;
use crate::locks::LockUser;
use crate::store::{IndexId, MemberKey};
use crate::views::Withheld;
use crate::writes::{self, Change, CommitError, Mailboxes};

pub(crate) use file::FileEngine;
pub(crate) use transaction::{Hold, Locks, Transaction};

/// A change the server makes to a hosted file: one request's work, as a
/// value. Who asks is part of it (the lock user, the name other users see
/// for it, the account and privilege set a commit is checked against, the
/// account op73's calculations see, the session and account an authoring
/// edit's locks and stamps name), so that it can be run by another engine,
/// recorded with its outcome and replayed (docs/server-storage.md 5, 7.2,
/// 7.3, 12).
#[derive(Clone, Debug, PartialEq)]
pub enum Operation {
    /// op71 HostCreateRecord.
    ReserveRecordIds(ReserveRecordIds),
    /// op72 HostGetSerial.
    AdvanceSerial(AdvanceSerial),
    /// op70 HostCommit.
    CommitRecords(CommitRecords),
    /// op60 Lock.
    TakeLock(TakeLock),
    /// op61 Upgrade.
    UpgradeLock(UpgradeLock),
    /// op62 Unlock.
    ReleaseLock(ReleaseLock),
    /// op65 KillAllLocks.
    ReleaseLocks(ReleaseLocks),
    /// op13 UploadToHost into the file.
    UploadToCatalog(UploadToCatalog),
    /// What an op86 program stores once it ran: its UploadMultiple and
    /// DeleteTable writes and what reads left waiting ([`Draft`]).
    ChangeSchema(ChangeSchema),
    /// The indexes finds built on demand ([`Draft`]).
    StoreIndexes(StoreIndexes),
    /// op73 HostUpdateTable.
    UpdateTable(UpdateTable),
    /// op88 UploadBinaryData.
    StoreContainerData(StoreContainerData),
    /// op87 AdjustReferenceCount.
    AdjustContainerCount(AdjustContainerCount),
    /// op240, OpenMaker's authoring extension: an edit of the file's
    /// design, boxed: its requests are larger than every other operation.
    EditDesign(Box<EditDesign>),
    /// op240's Save: write the file now if it holds changes the disk lacks,
    /// such as a client's op13 uploads, and answer whether the disk holds
    /// them all.
    SaveFile,
}

/// op71: reserve record ids and lock them for a transaction
/// (write-path.md 3.1; [`writes::create_record`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReserveRecordIds {
    pub user: LockUser,
    /// "session name (account)": what a lock conflict names.
    pub holder: String,
    pub request: CreateRecord,
    /// How many more locks the user's connection may take
    /// ([`writes::MAX_LOCKS`]; [`writes::UNCAPPED`] for a server script).
    pub room: usize,
    /// The new last issued id must be on disk before the ids are answered:
    /// a client's op71 (#927). A server script's reservation waits for the
    /// script's commit to be saved.
    pub durable: bool,
}

/// op72: read a field's next serial value, replacing or advancing it
/// (write-path.md 3.2; [`writes::get_serial`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvanceSerial {
    pub request: GetSerial,
}

/// op70: commit the records op14 staged (write-path.md 3.4, 5;
/// [`writes::commit`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRecords {
    pub user: LockUser,
    /// The account the session logged in as, which formulas limiting a
    /// record action read.
    pub account: String,
    /// The account's privilege set as it was at login.
    pub access: SetAccess,
    /// The commit list as uploaded.
    pub list: OwnedNode,
    /// The staged records the list names, out of the session's temporary
    /// area ([`writes::TempArea::lend`]).
    pub staged: Staged,
    /// The records of the list the session was served without their values
    /// (`views::Withheld::named_in`).
    pub withheld: Withheld,
}

/// Staged records by path `[T, 05, R]`.
pub type Staged = BTreeMap<KeyPath, OwnedNode>;

/// op60: take a lock without waiting (write-path.md 4.2; [`writes::lock`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakeLock {
    pub user: LockUser,
    /// "session name (account)": what a lock conflict names.
    pub holder: String,
    pub request: Lock,
    /// How many more locks the user's connection may take.
    pub room: usize,
}

/// op61: change the type of a lock (schema-path.md 4.2;
/// [`writes::upgrade`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeLock {
    pub user: LockUser,
    /// "session name (account)": what a lock conflict names.
    pub holder: String,
    pub request: Upgrade,
    /// How many more locks the user's connection may take.
    pub room: usize,
}

/// op62: release one lock, telling the other clients of a change it names
/// (write-path.md 4.3; [`writes::unlock`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseLock {
    /// The releasing connection; the request names its transaction.
    pub connection: u32,
    pub request: Unlock,
}

/// op65: release a transaction's locks, or all of a connection's
/// (write-path.md 4.4; [`writes::kill_all_locks`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseLocks {
    /// The releasing connection; the request names its transaction.
    pub connection: u32,
    pub request: KillAllLocks,
}

/// op13: put an uploaded subtree, or a single value, at its destination in
/// the file, replacing what was there (schema-path.md 3.3;
/// `Store::put_subtree`). The caller checked the session's Full Access;
/// op13 takes and checks no lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadToCatalog {
    /// The destination, never the whole file.
    pub path: KeyPath,
    pub node: OwnedNode,
}

/// What an op86 program stores once it ran, completed from
/// [`Draft::ChangeSchema`] (schema-path.md 3.4, query-program.md 7.1): the
/// program ran against the file under its read lock, and this is the rest
/// of the request. The caller checked the session's Full Access for a
/// program with schema instructions; they take and check no lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSchema {
    /// Its UploadMultiple and DeleteTable writes, in order
    /// (`Store::apply`).
    pub writes: Vec<om_query::Write>,
    /// The member keys GenerateMemberKey issued that were waiting
    /// (`Store::take_issued_member_keys`).
    pub member_keys: Vec<MemberKey>,
    /// The indexes finds built on demand that were waiting
    /// (`Store::take_indexes_on_demand`).
    pub indexes: Vec<IndexId>,
}

/// Store the indexes finds built on demand that were waiting
/// (query-program.md 7.1; `Store::store_indexes`), completed from a
/// [`Draft`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreIndexes {
    pub indexes: Vec<IndexId>,
}

/// An operation still to be completed with what reads left waiting in the
/// running server (the module doc's "Drafts"; [`FileEngine::submit_draft`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Draft {
    /// What an op86 program stores once it ran: its UploadMultiple and
    /// DeleteTable writes, with the member keys and indexes waiting.
    /// Completed as [`Operation::ChangeSchema`], or as
    /// [`Operation::StoreIndexes`] when there are no writes and no member
    /// keys to write.
    ChangeSchema { writes: Vec<om_query::Write> },
    /// What a server script's find or related-record lookup stores: the
    /// indexes waiting, as [`Operation::StoreIndexes`]. Member keys wait on,
    /// as they did.
    StoreIndexes,
}

/// op73: do the field work the client listed for a table, recomputing the
/// stored calculations the change made stale (schema-path.md 3.6, 7.2;
/// `Store::update_table`). The other clients learn of it when the client
/// releases the table's lock, after the reply (schema-path.md 9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateTable {
    /// The table key `[T]`.
    pub table: Vec<u8>,
    /// The account that saved the change, which calculations reading
    /// `Get ( AccountName )` see.
    pub account: String,
}

/// op88: store uploaded container data in a table's library, or in the
/// file's own (containers.md 3.4; `containers::store_upload`). The caller
/// checked the session's privileges and read the upload within the memory
/// the client may hold; the data travels here, and the charge for it is
/// given back once the operation's outcome is returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreContainerData {
    /// The table key `[T]` naming the library, none for the file's own.
    pub table: KeyPath,
    /// The data read from the upload, or why it is not valid binary data,
    /// which is answered without touching the tree; the file is saved
    /// after it all the same, as after every op88 that was read.
    pub data: Result<BinaryData, String>,
    /// The upload's size in bytes, for the log.
    pub bytes: usize,
}

/// op87: add a delta to a library member's reference count, deleting the
/// member at 0 (containers.md 5.3; `containers::adjust_reference_count`).
/// The caller checked the session's privileges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdjustContainerCount {
    pub request: AdjustReferenceCount,
}

/// op240: one edit of the file's design (docs/authoring.md, "How writes
/// reach the file"; `authoring::edit`). The caller checked that the
/// session's account may author; the edit checks the file, holds the locks
/// FileMaker Pro holds for the same change while it is stored, stamps what
/// it changes with the environment's time and gives new objects UUIDs from
/// its random source.
#[derive(Clone, Debug, PartialEq)]
pub struct EditDesign {
    /// The connection asking: the edit's locks are its own, under the
    /// authoring extension's transaction, and its client is not told of
    /// the change.
    pub connection: u32,
    /// The client's session name: the holder other users see while the
    /// edit holds its locks, and the stamp's user name.
    pub session_name: String,
    /// The account the session logged in as: the stamp's account.
    pub account: String,
    pub edit: DesignEdit,
}

/// An authoring edit's request as decoded (`om_protocol::authoring`).
#[derive(Clone, Debug, PartialEq)]
pub enum DesignEdit {
    ReplaceScriptSteps(design::ReplaceScriptSteps),
    CreateField(design::CreateField),
    PlaceField(design::PlaceField),
    PlaceText(design::PlaceText),
    PlacePortal(design::PlacePortal),
    PlaceButton(design::PlaceButton),
    PlaceButtonStep(design::PlaceButtonStep),
    SetObjectFrames(design::SetObjectFrames),
    DeleteObjects(design::DeleteObjects),
    SetText(design::SetText),
    FieldControl(design::FieldControl),
    PartOptions(design::PartOptions),
    ChangeParts(design::ChangeParts),
    LayoutWidth(design::LayoutWidth),
    LayoutSetup(design::LayoutSetup),
    CreateLayout(design::CreateLayout),
    DuplicateLayout(design::DuplicateLayout),
    DeleteLayout(design::DeleteLayout),
    ValueListValues(design::ValueListValues),
}

/// What an operation answers, one shape per operation; the caller encodes
/// it for the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    ReserveRecordIds(CreateRecordReply),
    AdvanceSerial(GetSerialReply),
    CommitRecords(CommitReply),
    TakeLock(LockReply),
    UpgradeLock(LockReply),
    /// op62 is answered with the request's own header (write-path.md 4.3).
    ReleaseLock,
    /// op65 is answered with the request's own header (write-path.md 4.4).
    ReleaseLocks,
    /// Whether the subtree was stored: it is not on a read-only file or
    /// where the file cannot hold it. op13's reply is the same either way
    /// (schema-path.md 3.3).
    UploadToCatalog(bool),
    /// op86's reply is what its program answered when it ran; this is
    /// only whether its writes were refused because the file became
    /// read-only after it ran, which fails the request.
    ChangeSchema(Result<(), &'static str>),
    /// A find's reply is what it answered when it ran.
    StoreIndexes,
    /// Whether the work changed the file, or why it was refused, which
    /// leaves the file hosted read-only (`Store::update_table`).
    UpdateTable(Result<bool, &'static str>),
    /// op88's reply as the wire has it (containers.md 3.2).
    StoreContainerData(UploadBinaryReply),
    /// The member's count after the change, or `COUNT_FAILED`.
    AdjustContainerCount(u32),
    EditDesign(Edited),
    /// Whether the disk holds every change the file holds.
    SaveFile(Result<(), FileNotSaved>),
}

/// op240's reply: the edit's answer, and for an edit that changed the file,
/// what the storage error says when the change cannot be stored
/// ([`Outcome::not_stored`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edited {
    pub reply: design::Reply,
    pub not_stored: Option<NotStored>,
}

/// What an edit's storage error says: what was not done, for the log
/// ("script 3 not saved"), and what the file keeps, for the reply
/// ("script 3 is unchanged").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotStored {
    pub not_done: String,
    pub kept: String,
}

/// Why op240's Save leaves changes off the disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileNotSaved {
    /// The file is hosted read-only: nothing is written to it.
    ReadOnly,
    /// The save failed: the changes wait in memory for the next one.
    Failed,
}

/// op70's reply: whether the commit was written, and the staged records it
/// did not take, which go back to the session's temporary area
/// ([`writes::TempArea::settle`]). The reply on the wire has no status
/// (write-path.md 3.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitReply {
    pub result: Result<(), CommitError>,
    pub unused: Staged,
}

/// An operation's reply and the notices for other clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub reply: Reply,
    pub notices: Notices,
}

/// The changes to tell the connections registered on the file about
/// (write-path.md 6), all but the one whose request made them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notices {
    pub from: u32,
    pub changes: Vec<Change>,
}

impl Notices {
    /// Queue them for the connections registered on `file`.
    pub fn deliver(&self, file: &Hosted, mailboxes: &Mailboxes) {
        if !self.changes.is_empty() {
            writes::notify(file, mailboxes, self.from, &self.changes);
        }
    }
}

impl Outcome {
    fn new(reply: Reply) -> Self {
        Outcome {
            reply,
            notices: Notices::default(),
        }
    }

    /// The outcome when what the operation did could not be stored: the
    /// storage error in place of its reply, and no notices
    /// (docs/server-storage.md 5). op70 fails with the save's message
    /// (`writes::CommitError::NotSaved`), op71 and an authoring edit with
    /// the save's FileMaker error ([`crate::hosting::save_error`]), the
    /// edit's message saying what the file keeps.
    fn not_stored(self, error: &std::io::Error) -> Outcome {
        let reply = match self.reply {
            Reply::EditDesign(Edited {
                not_stored: Some(not_stored),
                ..
            }) => Reply::EditDesign(Edited {
                reply: design::Reply::Refused {
                    status: crate::hosting::save_error(error),
                    message: format!(
                        "The file could not be saved ({error}); {}.",
                        not_stored.kept
                    ),
                    current: None,
                },
                not_stored: None,
            }),
            Reply::ReserveRecordIds(_) => Reply::ReserveRecordIds(writes::refused_ids(u32::from(
                crate::hosting::save_error(error),
            ))),
            Reply::CommitRecords(reply) => Reply::CommitRecords(CommitReply {
                result: Err(CommitError::NotSaved(format!(
                    "commit of {} record(s) taken back: the file could not be saved: {error}",
                    self.notices.changes.len()
                ))),
                unused: reply.unused,
            }),
            other => other,
        };
        Outcome {
            reply,
            notices: Notices {
                from: self.notices.from,
                changes: Vec::new(),
            },
        }
    }
}

/// What an operation may read of the world besides the file: the clock and
/// a random source, so that two engines can be given the same seeded source
/// (docs/server-storage.md 12). [`run`] itself reads them only through
/// this: the time and the UUID of a write's record environment, an
/// authoring edit's stamps and the UUIDs of what it adds, and the unique
/// ids of container data op73's recalculation and op88 store. The
/// calculation engine below it does not yet: a formula an operation
/// evaluates (a privilege set's record limit, `writes::refused_action`;
/// the stored calculations op73 recomputes, `Store::update_table`) takes
/// today's date, `Random` and its time budget from the process
/// (`om_data::rules`, `om_calc::eval::CalcHost::random`), so an operation
/// whose formulas read them need not replay to the same reply or tree
/// ([#692](https://github.com/OpenMaker-Dev/openmaker/issues/692)).
pub trait Environment: Send + Sync {
    /// The host's time now, in its time zone.
    fn now(&self) -> jiff::Zoned;
    /// Fill `bytes` from the random source.
    fn fill_random(&self, bytes: &mut [u8]);
}

/// The host's clock and the system's random source.
#[derive(Debug, Default)]
pub struct HostEnvironment;

impl Environment for HostEnvironment {
    fn now(&self) -> jiff::Zoned {
        jiff::Zoned::now()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        use ring::rand::SecureRandom as _;
        ring::rand::SystemRandom::new()
            .fill(bytes)
            .expect("the system random source is available");
    }
}

/// A random UUID in `Get ( UUID )`'s form, from `environment`.
pub fn uuid(environment: &dyn Environment) -> String {
    let mut bytes = [0u8; 16];
    environment.fill_random(&mut bytes);
    crate::recalc::uuid_text(bytes)
}

/// Run `operation` on the file named `file` (for log lines), against what
/// the engine holds of it (`hold`) and its lock table (`locks`), taking
/// the clock and randomness from `environment` (formulas it evaluates
/// excepted, [`Environment`]). It sends nothing,
/// saves nothing and takes no lock of its own: the effects are what it did
/// to the transaction and the lock table, and the engine stores them
/// before the outcome may go out. A refusal can keep effects: op71 refused
/// for a lock another user holds has used up its ids (write-path.md 3.1).
pub fn run(
    operation: Operation,
    file: &str,
    hold: Hold<'_, '_>,
    locks: &mut Locks<'_>,
    environment: &dyn Environment,
) -> Outcome {
    match (operation, hold) {
        (Operation::ReserveRecordIds(op), Hold::Transaction(tx)) => Outcome::new(
            Reply::ReserveRecordIds(writes::create_record(op, file, tx, locks)),
        ),
        (Operation::AdvanceSerial(op), Hold::Transaction(tx)) => {
            Outcome::new(Reply::AdvanceSerial(writes::get_serial(&op.request, tx)))
        }
        (Operation::CommitRecords(op), Hold::Transaction(tx)) => {
            let from = op.user.connection;
            let (reply, changes) = writes::commit(op, file, tx, locks, environment);
            Outcome {
                reply: Reply::CommitRecords(reply),
                notices: Notices { from, changes },
            }
        }
        (Operation::TakeLock(op), Hold::View(store)) => {
            Outcome::new(Reply::TakeLock(writes::lock(op, file, store, locks)))
        }
        (Operation::TakeLock(op), Hold::Transaction(tx)) => {
            Outcome::new(Reply::TakeLock(writes::lock(op, file, tx.view(), locks)))
        }
        (Operation::UpgradeLock(op), _) => {
            Outcome::new(Reply::UpgradeLock(writes::upgrade(op, file, locks)))
        }
        (Operation::ReleaseLock(op), _) => {
            let from = op.connection;
            Outcome {
                reply: Reply::ReleaseLock,
                notices: Notices {
                    from,
                    changes: writes::unlock(op, locks),
                },
            }
        }
        (Operation::ReleaseLocks(op), _) => {
            writes::kill_all_locks(op, locks);
            Outcome::new(Reply::ReleaseLocks)
        }
        (Operation::UploadToCatalog(op), Hold::Transaction(tx)) => {
            Outcome::new(Reply::UploadToCatalog(schema::upload_to_catalog(op, tx)))
        }
        (Operation::ChangeSchema(op), Hold::Transaction(tx)) => {
            Outcome::new(Reply::ChangeSchema(schema::change_schema(op, tx)))
        }
        (Operation::StoreIndexes(op), Hold::Transaction(tx)) => {
            tx.store_indexes(op.indexes);
            Outcome::new(Reply::StoreIndexes)
        }
        (Operation::UpdateTable(op), Hold::Transaction(tx)) => Outcome::new(Reply::UpdateTable(
            schema::update_table(op, tx, environment),
        )),
        (Operation::StoreContainerData(op), Hold::Transaction(tx)) => Outcome::new(
            Reply::StoreContainerData(crate::containers::store_upload(op, file, tx, environment)),
        ),
        (Operation::AdjustContainerCount(op), Hold::Transaction(tx)) => {
            Outcome::new(Reply::AdjustContainerCount(
                crate::containers::adjust_reference_count(&op.request, file, tx),
            ))
        }
        (Operation::EditDesign(op), Hold::Transaction(tx)) => {
            let from = op.connection;
            let (reply, changes) = crate::authoring::edit(*op, file, tx, locks, environment);
            Outcome {
                reply: Reply::EditDesign(reply),
                notices: Notices { from, changes },
            }
        }
        // The engine saves the file (`FileEngine::submit`).
        (Operation::SaveFile, Hold::Transaction(tx)) => {
            Outcome::new(Reply::SaveFile(if tx.is_read_only() {
                Err(FileNotSaved::ReadOnly)
            } else {
                Ok(())
            }))
        }
        (operation, _) => not_held(operation, file),
    }
}

/// The refusal of an operation run without the hold of the file it needs:
/// an engine's mistake, refused with FileMaker error 3 and logged.
fn not_held(operation: Operation, file: &str) -> Outcome {
    error!("{file:?}: an operation ran without the hold of the file it needs: {operation:?}");
    let refused_lock = LockReply::Refused {
        status: writes::LOCK_UNAVAILABLE,
        conflict: Conflict {
            self_conflict: false,
            users: String::new(),
        },
    };
    Outcome::new(match operation {
        Operation::ReserveRecordIds(_) => {
            Reply::ReserveRecordIds(writes::refused_ids(writes::UNAVAILABLE))
        }
        Operation::AdvanceSerial(_) => Reply::AdvanceSerial(GetSerialReply {
            conflict: None,
            status: writes::UNAVAILABLE,
            value: String::new(),
        }),
        Operation::CommitRecords(op) => Reply::CommitRecords(CommitReply {
            result: Err(CommitError::NotSaved(
                "commit refused: the engine did not hold the file".into(),
            )),
            unused: op.staged,
        }),
        Operation::TakeLock(_) => Reply::TakeLock(refused_lock),
        Operation::UpgradeLock(_) => Reply::UpgradeLock(refused_lock),
        Operation::ReleaseLock(_) => Reply::ReleaseLock,
        Operation::ReleaseLocks(_) => Reply::ReleaseLocks,
        Operation::UploadToCatalog(_) => Reply::UploadToCatalog(false),
        Operation::ChangeSchema(_) => Reply::ChangeSchema(Err(NOT_HELD)),
        Operation::StoreIndexes(_) => Reply::StoreIndexes,
        Operation::UpdateTable(_) => Reply::UpdateTable(Err(NOT_HELD)),
        Operation::StoreContainerData(_) => {
            Reply::StoreContainerData(crate::containers::upload_reply(
                writes::UNAVAILABLE as u16,
                Vec::new(),
                NOT_HELD.into(),
            ))
        }
        Operation::AdjustContainerCount(_) => Reply::AdjustContainerCount(COUNT_FAILED),
        Operation::EditDesign(_) => Reply::EditDesign(Edited {
            reply: design::Reply::Refused {
                status: design::status::UNSUPPORTED,
                message: NOT_HELD.into(),
                current: None,
            },
            not_stored: None,
        }),
        Operation::SaveFile => Reply::SaveFile(Err(FileNotSaved::Failed)),
    })
}

const NOT_HELD: &str = "the engine did not hold the file";
