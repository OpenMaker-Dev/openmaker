//! The engine interface (`docs/server-storage.md` 5): every change the
//! server makes to a hosted file or its lock table is an [`Operation`], a
//! value that owns its data, [`run`] against what the engine holds of the
//! file, and answered with an [`Outcome`]: the reply and the notices for
//! other clients. Effects are what the run did to the [`Transaction`] and
//! the [`Locks`]. The file engine ([`FileEngine`]) stores them, and only
//! then does the caller encode the reply and deliver the notices.
//!
//! The operations: commit records (op70), reserve record ids (op71),
//! advance a serial (op72), take, upgrade and release locks (op60, op61,
//! op62, op65), upload to a catalog (op13), store what an op86 program's
//! schema instructions changed (UploadMultiple, GenerateMemberKey,
//! DeleteTable), store the indexes a find built on demand, update a table
//! (op73), store container data and adjust a library member's reference
//! count (op88, op87), an authoring edit and the authoring extension's
//! save (op240); a server script's commit, its Set Next Serial Value, its
//! locks on a record and on the accounts catalog and their releases, and
//! the save at its end (op240's save again); the release of a session's
//! locks when it ends, logs out of a file or closes it; the account edits
//! (Change Password, Reset Account Password, Enable Account, Delete
//! Account); finishing the deferred field work a file is opened with, and
//! its save (op240's save again); and keeping a window's found set in a
//! local file.
//!
//! Only the engine can change a file: the store's write guard and the
//! lock table for changing take a [`Writer`], which only this module can
//! make, `Hosted::lock_table` gives everyone else the table to read, and
//! a whole-file copy is an export ([`export`]), which opens the file on
//! disk only to read it (`Store::open_saved`).
//! What else takes the store for writing is the setup of a file no one can
//! reach yet (`hosting::load_hosted`) and test switches
//! (`Hosted::fail_commit_saves`, `Writer::for_tests`). The file's path and
//! folder (`Hosted::path`, `Hosted::at`) cannot be kept from being opened
//! for writing; docs/server-storage.md 5 lists who reads them.
//!
//! # The environment
//!
//! What an operation reads of the world besides the file comes from the
//! [`Environment`]: the clock (`now`), the random source (`fill_random`,
//! `try_fill_random`) and the stopwatch of its formulas' time budgets. A
//! replay on a seeded environment gives the same outcome only if every
//! operation draws the same bytes in the same order, so each draws in this
//! order and only when it gets that far (`tests`,
//! `each_kind_of_operation_draws_from_its_environment_as_written_here`,
//! checks one of each against this list):
//!
//! - a record environment ([`crate::recalc::record_environment_in`]): 16
//!   bytes for its UUID, one clock reading, 8 bytes for the seed of
//!   `Random`'s generator, then a stopwatch;
//! - container data that becomes a new library member (`library::store`):
//!   16 bytes for the library's own unique id when the library is new,
//!   then 16 for the member's; data the library already holds draws
//!   nothing;
//! - op70: one record environment, when it checks the first listed record
//!   that exists or that it creates;
//! - op73, and the deferred field work finished at opening (table by
//!   table): one record environment for each recalculation, and one more
//!   for each record when its calculations read `Get ( UUID )`, the record
//!   keeping the first one's `Random` and stopwatch; the container data it
//!   stores, as it reaches it;
//! - op88: the container data it stores;
//! - a server script's commit: the container data of each record, record
//!   by record, cell by cell;
//! - an authoring edit: one clock reading for its stamps once its checks
//!   passed, then 16 bytes for each UUID of what it adds, in the order it
//!   adds them;
//! - Change Password and Reset Account Password: one clock reading once
//!   the new password is no longer than 100 characters; then, once the
//!   account is found and its kind and status (for Change Password also
//!   its old password and its privilege set's rules) are checked, the 10
//!   bytes of the salt (`try_fill_random`, whose failure refuses the
//!   edit). The account's name, its stored checksum and, for a stamped
//!   edit, the catalog's index are checked after the salt is drawn, as
//!   the member is rewritten: a refusal there has drawn it;
//! - Enable Account: one clock reading for the stamp, when a stamped edit
//!   changes the account's state, before its checksum is checked; Delete
//!   Account: one clock reading for the stamp, once the catalog's index
//!   reads as FileMaker keeps it;
//! - the other operations draw nothing, and neither does any operation
//!   refused unrun because the file takes no more changes
//!   ([`FileEngine::submit`]).
//!
//! # Credentials
//!
//! The passwords an account edit was given are not part of its operation:
//! they travel beside it, as typed, to [`run`] alone ([`Credentials`],
//! [`FileEngine::submit_with`]). So whatever keeps an operation, a
//! recording now and a log or another server later, has the operation
//! without them, and a replay supplies them again. [`Password`] is held in
//! memory only, is not `Clone`, is wiped when dropped, and prints only a
//! placeholder; the run checks the old one against the stored verifier and
//! drops it, checks the new one against the privilege set's rules, hashes
//! it with a salt from the [`Environment`] and drops it, all under the
//! store. No log line names a password. A durable log keeps an account
//! edit's effects (the account member as rewritten, with its salted
//! verifier, as the file holds it), and its history only the fact that
//! Access paths changed (docs/server-storage.md 9).
//!
//! # Lock order
//!
//! [`FileEngine::submit`] takes, in this order and only what the operation
//! needs: the file's store (`Hosted::read_store` for op60,
//! `Hosted::write_store` for op70, op71, op72, the schema operations,
//! op87, op88, op240, a server script's commit and its Set Next Serial
//! Value, the account edits, finishing deferred field work and keeping a
//! found set), then its lock table (`Hosted::locks`, when the operation
//! first uses it, [`Locks`]; the schema operations, op87, op88, op240's
//! save, Set Next Serial Value, the account edits, deferred work and a
//! found set never do; a server script's commit takes it first thing,
//! [`Locks::hold`], as it always held both from its start). op70, a
//! client's op71 and a server script's commit, which take the lock table,
//! hold it with the store while they save and let go of it first, then
//! write the log lines that say what was stored
//! ([`Transaction::once_stored`]) under the store alone, where they wrote
//! them under both; dropping the store's write guard takes the file's
//! accounts while it still holds the store (`hosting::StoreWriteGuard`),
//! which is when a changed password counts for the next login. op61, op62
//! and op65 take only the lock table, as before, and so do a server
//! script's lock and its release and the release of a session's locks, as
//! the script host and a connection's end took it: a connection's end
//! releases its locks one file at a time, each under that file's lock
//! table alone, before it removes the file's registration. A server
//! script's account step takes its lock on the account catalog, then
//! submits the edit, then releases the lock, which tells the other
//! clients of the change: lock table, store, lock table, each let go
//! before the next is taken, as it always has. A found set is kept only
//! when no one holds the store ([`FileEngine::try_submit`]): a window
//! closing never waits on a request. Deferred field work is finished as a
//! file is loaded, before anyone else can reach it.
//! [`FileEngine::submit_draft`] completes its draft right after it takes
//! the store, from state the write guard gives it alone (see "Drafts"). An
//! authoring edit (op240) takes the lock table only while it takes its
//! locks, and again while it releases them when it is refused after taking
//! them; it saves under the store alone, and its locks are released after
//! the engine lets go of the store, the lock table taken on its own for
//! that ([`Locks::lock_while_stored`]), as op240 always held them. The one
//! other lock taken inside `submit` is the file's thumbnail cache
//! (`Hosted::thumbnails`): under the store once op86's DeleteTable that
//! took a table's container library is stored, where it was taken before
//! the save, and under the store and the lock table once a server script's
//! commit that left a table without records is stored; store, then
//! thumbnails, as op96 takes them. A save that fails is answered under
//! what the operation held: taking its change back (`Store::roll_back`)
//! needs the store alone, and releasing the locks it took
//! ([`Locks::take_back`]) the lock table, taken again after the store if
//! the run let it go. Its alert takes the event log's own lock under the
//! store, as a failed save's alert always did. The no-more-changes state
//! of a save whose outcome is unknown is the store's (read and set under
//! its write lock, `Store::writes_refused`) and the file's
//! (`Hosted::writes_refused`, set once under the store and read without a
//! lock, so that the administrator's list of files never waits for the
//! store). The file's own lock on disk (`crate::filelock`), which the store
//! holds for as long as the file is hosted and a save moves to the new
//! copy, is not one of these: it is never waited for. Nothing
//! else: the caller delivers notices after it returns, taking the file's
//! registrations and then the mailboxes (`writes::notify`), and a session's
//! own state (its temporary area, the records it was served withheld, the
//! memory its uploads are charged, a server script's waiting container
//! data) is read before and settled after. This is the order the write path
//! has always used (store, lock table, registrations, mailboxes), with the
//! last two no longer inside the first two.
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

#[cfg(debug_assertions)]
mod checking;
#[cfg(debug_assertions)]
mod copies;
pub(crate) mod export;
mod file;
mod overlay;
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the store engine's first code (docs/server-storage.md 5): no file is hosted on it yet"
    )
)]
pub(crate) mod paged;
mod reading;
mod schema;
mod state;
mod transaction;
mod view;

/// Debug builds only: they test switches only those have (failed saves,
/// the test view), as `export`'s tests do.
#[cfg(all(test, debug_assertions))]
mod tests;

use std::borrow::Cow;
use std::collections::BTreeMap;

use log::error;
use om_data::host::security::SetAccess;
use om_hbam::{OwnedNode, RidList};
use om_protocol::authoring as design;
use om_protocol::container::{AdjustReferenceCount, UploadBinaryReply, COUNT_FAILED};
use om_protocol::schema::Upgrade;
use om_protocol::write::{
    Conflict, CreateRecord, CreateRecordReply, GetSerial, GetSerialReply, KeyPath, KillAllLocks,
    Lock, LockReply, Unlock, LOCK_WRITE,
};

use crate::hosting::{AccountEditError, AccountRef, Hosted, PasswordChangeError};
use crate::library::BinaryData;
use crate::locks::LockUser;
use crate::store::{IndexId, MemberKey};
use crate::views::Withheld;
use crate::writes::{self, Change, CommitError, Mailboxes, RecordWrite};

pub(crate) use export::{Destination, ExportError, OnDisk, Progress, Source};
pub(crate) use file::FileEngine;
pub(crate) use overlay::{Cover, Overlay};
pub(crate) use reading::{assert_no_reading_held, Read, Waiting};
#[cfg(test)]
pub(crate) use transaction::written;
pub(crate) use transaction::{Hold, Locks, Transaction};
/// For views written in tests outside the engine.
#[cfg(test)]
pub(crate) use view::Step;
pub(crate) use view::{
    wire, Child, Direction, Head, Noted, ReadError, TreeView, Version, View, Wiped, NO_LIMIT,
};

/// A change the server makes to a hosted file: one request's work, as a
/// value. Who asks is part of it (the lock user, the name other users see
/// for it, the account and privilege set a commit is checked against, the
/// account op73's calculations see, the session and account an authoring
/// edit's locks and stamps name, whether an account edit may manage the
/// account and who stamps it), so that it can be run by another engine,
/// recorded with its outcome and replayed (docs/server-storage.md 5, 7.2,
/// 7.3, 12). Its `Debug` shows no password ([`Password`]).
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
    /// them all. A server script's end submits it too, for what its
    /// reservations left unsaved (`scripts::host`, `ServerHost::close`).
    SaveFile,
    /// A server script's commit of its records.
    CommitScriptRecords(CommitScriptRecords),
    /// A server script's Set Next Serial Value.
    SetNextSerial(SetNextSerial),
    /// A server script's lock on a record or on the accounts catalog.
    TakeScriptLock(TakeScriptLock),
    /// The release of a server script's locks on one path.
    ReleaseScriptLock(ReleaseScriptLock),
    /// The release of a session's locks when it ends: a client's
    /// connection that ended, logged out of the file or closed it, or a
    /// server script's session.
    ReleaseSessionLocks(ReleaseSessionLocks),
    /// Change Password (83), a server script's or the desktop's own.
    ChangePassword(ChangePassword),
    /// Reset Account Password (136).
    ResetPassword(ResetPassword),
    /// Enable Account (137).
    EnableAccount(EnableAccount),
    /// Delete Account (135).
    DeleteAccount(DeleteAccount),
    /// Finish the deferred field work the file records as pending
    /// (schema-path.md 7.2; `Store::recover`), and save it: a server's
    /// file as it is loaded, before it is served, a local file when an
    /// account it admits first logs in (`Hosted::recover`), so that closing
    /// a local file before that leaves the file as it was.
    FinishDeferredWork,
    /// Keep the found set and record a window of a local file closes on
    /// (file-metadata.md 11; `LocalHost::save_found_set`).
    SaveFoundSet(SaveFoundSet),
}

/// A password an account edit was given, as it was typed: held in memory
/// only, never copied (not `Clone`), wiped when dropped, and never shown:
/// its `Debug` is a placeholder (the module doc's "Credentials").
pub struct Password(zeroize::Zeroizing<String>);

impl Password {
    pub fn new(text: &str) -> Self {
        Password(zeroize::Zeroizing::new(text.to_owned()))
    }

    /// The password, for the run that checks or hashes it.
    pub(crate) fn text(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(..)")
    }
}

/// The passwords that travel beside an operation to [`run`], never in it
/// (the module doc's "Credentials"): none for every operation but Change
/// Password and Reset Account Password.
pub enum Credentials {
    None,
    /// [`Operation::ChangePassword`]'s old and new password.
    Change {
        old: Password,
        new: Password,
    },
    /// [`Operation::ResetPassword`]'s new password.
    Reset {
        new: Password,
    },
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Credentials::None => "Credentials::None",
            Credentials::Change { .. } => "Credentials::Change(..)",
            Credentials::Reset { .. } => "Credentials::Reset(..)",
        })
    }
}

/// Who makes an account edit that stamps the account catalog as a
/// catalog commit does (schema-path.md 8.1): the user and account names
/// the modification stamps carry. The edit raises no notices: a server
/// script tells the other clients of the change when it releases its lock
/// on the catalog ([`ReleaseScriptLock`], schema-path.md 9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountStamp {
    pub user: String,
    pub account: String,
}

/// Change Password: set an account's password from the old one to a new
/// one, which travel beside the operation ([`Credentials::Change`]), as
/// FileMaker's `SetPassword` sets the signed-in account's own
/// (local-files.md 2.4; `hosting::change_password`). The caller cut the
/// passwords as the step cuts them and checked who may ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangePassword {
    pub account: AccountRef,
    /// The caller may manage the account (a script with full access): the
    /// privilege set's rules on changing, length and reuse are skipped.
    pub manager: bool,
    /// None: no stamps (the desktop's own Change Password).
    pub stamp: Option<AccountStamp>,
}

/// Reset Account Password (136): the account named `account` given the
/// password that travels beside the operation ([`Credentials::Reset`])
/// (local-files.md 2.4; `hosting::reset_account_password`). The caller
/// checked that the session may manage accounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetPassword {
    pub account: String,
    /// A change is due at the account's next login.
    pub expire: bool,
    pub stamp: Option<AccountStamp>,
}

/// Enable Account (137): the account named `account` made active or
/// inactive (local-files.md 2.4; `hosting::enable_account`). The caller
/// checked that the session may manage accounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnableAccount {
    pub account: String,
    pub active: bool,
    pub stamp: Option<AccountStamp>,
}

/// Delete Account (135): the account named `account` removed
/// (local-files.md 2.4 and 3; `hosting::delete_account`). The caller
/// checked that the session may manage accounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteAccount {
    pub account: String,
    pub stamp: Option<AccountStamp>,
}

/// Keep, at table occurrence `occurrence`'s `[TO, 07]`, the found set and
/// record a window of a local file closes on with table `table`
/// (file-metadata.md 11; `om_data::meta::saved_found_set_written`), at the
/// table's data version, which the run reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveFoundSet {
    /// The occurrence's key `TO`.
    pub occurrence: Vec<u8>,
    /// The table's key `T`.
    pub table: Vec<u8>,
    /// The found records; `None` for every record of the table.
    pub records: Option<RidList>,
    /// The current record's id; `None` for none.
    pub current: Option<u64>,
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
/// `schema::upload_to_catalog`). The caller checked the session's Full Access;
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
    /// (`schema::change_schema`).
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

/// A server script's commit (write-path.md 3.4, 5;
/// [`writes::commit_records`]): its records as they will be stored, each
/// with the container data its steps set, checked and written all or none.
/// The script host applied the records' rules (auto-enter, stored
/// calculations, validation) and checked the session's privileges before:
/// the run reads no account and no privilege set. The container data and
/// reference counts are part of the commit, saved and taken back with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitScriptRecords {
    /// The script session's lock user: the records' locks are its own, and
    /// its connection, which no client has, sends the notices, so every
    /// client registered on the file is told, the one that asked for the
    /// script included.
    pub user: LockUser,
    pub records: Vec<RecordWrite>,
    /// Delete All Records or Truncate Table: a table they leave without
    /// records loses its library.
    pub bulk_delete: bool,
}

/// A server script's Set Next Serial Value (script-runtime.md, "Steps";
/// [`writes::set_next_serial`]): a field's next serial value replaced in
/// a commit, saved before it counts as a record commit is. The script host
/// checked the session's [Full Access].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetNextSerial {
    /// The table key `[T]`.
    pub table: Vec<u8>,
    /// The field key `[F]`.
    pub field: Vec<u8>,
    pub value: String,
}

/// A server script's write lock on a record it changes, or on the accounts
/// catalog `[23, 1]` while an account step changes it (script-runtime.md
/// 2.3, local-files.md 2.4). Its rules are not op60's: it reads nothing of
/// the store (no modification count), no cap applies, and the script host
/// answers a conflict with the step's error (301 for a record, 310 for the
/// accounts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakeScriptLock {
    pub user: LockUser,
    /// "server name (script engine, account)": what a conflict names.
    pub holder: String,
    pub path: KeyPath,
}

/// The release of every lock a server script's lock user holds on a path
/// (`LockTable::release_path`): a record it left unchanged, or the accounts
/// catalog after an account step, whose release tells the other clients of
/// the `changes` the step made (schema-path.md 9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseScriptLock {
    pub user: LockUser,
    pub path: KeyPath,
    pub changes: Vec<Change>,
}

/// The release of every lock a session's connection holds in the file
/// (`LockTable::release_connection`): a client's connection that ended,
/// logged out of the file or closed it, or a server script's session that
/// ended or was aborted (docs/server-storage.md 7.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseSessionLocks {
    pub connection: u32,
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
    /// The subtree stored (`true`; the engine's refusal of an operation it
    /// does not hold aside, never `false`); or why it was not: the storage
    /// error of a save that failed, or the refusal of a file hosted
    /// read-only or of a destination the file cannot hold, which op13
    /// answers as a save that failed (#737).
    UploadToCatalog(Result<bool, StorageError>),
    /// op86's reply is what its program answered when it ran; this is
    /// only whether its writes were refused because the file became
    /// read-only after it ran, or were not stored, which fails the
    /// request.
    ChangeSchema(Result<(), SchemaNotChanged>),
    /// A find's reply is what it answered when it ran; this is whether the
    /// indexes it left waiting were stored. Those that were not wait again.
    StoreIndexes(Result<(), StorageError>),
    /// Whether the work changed the file, or why it was refused, which
    /// leaves the file hosted read-only (`Store::update_table`), or why it
    /// was not stored.
    UpdateTable(Result<bool, String>),
    /// op88's reply as the wire has it (containers.md 3.2).
    StoreContainerData(UploadBinaryReply),
    /// The member's count after the change, or `COUNT_FAILED`.
    AdjustContainerCount(u32),
    EditDesign(Edited),
    /// Whether the disk holds every change the file holds.
    SaveFile(Result<(), FileNotSaved>),
    /// Whether the commit was written, or FileMaker's error and why.
    CommitScriptRecords(Result<(), (u32, String)>),
    /// Whether the value was set, or FileMaker's error.
    SetNextSerial(Result<(), u32>),
    /// Whether the lock was taken, or the holders that keep it.
    TakeScriptLock(Result<(), Conflict>),
    ReleaseScriptLock,
    ReleaseSessionLocks,
    /// Why the password was not changed, with `SetPassword`'s codes.
    ChangePassword(Result<(), PasswordChangeError>),
    ResetPassword(Result<(), AccountEditError>),
    /// Whether the account changed: one already in that state is left as
    /// it is.
    EnableAccount(Result<bool, AccountEditError>),
    DeleteAccount(Result<(), AccountEditError>),
    /// Whether the work changed the file. Work that cannot be done is
    /// kept, and the file is hosted read-only (`Store::recover`); so is
    /// work that could not be stored, which is taken back.
    FinishDeferredWork(Result<bool, StorageError>),
    /// Whether the file holds the window's found set: not on a read-only
    /// file, for a table without a data version, where the occurrence
    /// holds one FileMaker Pro never wrote that way, nor when the save
    /// failed.
    SaveFoundSet(bool),
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileNotSaved {
    /// The file is hosted read-only: nothing is written to it.
    ReadOnly,
    /// The save failed before it wrote the file: the changes wait in memory
    /// for the next one.
    Failed(StorageError),
    /// Whether the file on disk holds them is unknown: this save, or an
    /// earlier one, renamed its copy over the file and could not flush the
    /// folder, and the file takes no more changes until it is hosted again.
    Unknown(StorageError),
}

/// Why op86's schema instructions changed nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaNotChanged {
    /// Refused before they ran: the file became read-only after the program
    /// ran, or an engine's mistake.
    Refused(&'static str),
    /// Not stored: the storage error.
    NotStored(StorageError),
}

/// What a reply says when an operation's change could not be stored, or
/// may not have been (docs/server-storage.md 2, rule 4; 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageError {
    /// FileMaker's error for it ([`crate::hosting::save_error`]): 800, 809
    /// or 810.
    pub code: u16,
    /// What happened, for a log line or the reply's text.
    pub message: String,
    /// The file may hold the change: the save renamed its copy over the
    /// file and could not flush the folder. The file then takes no more
    /// changes until it is hosted again.
    pub unknown: bool,
}

impl StorageError {
    /// The error of a save that failed with `error`.
    pub(crate) fn of(error: &crate::store::SaveError) -> Self {
        StorageError {
            code: crate::hosting::save_error(error.error()),
            message: error.to_string(),
            unknown: error.is_unknown(),
        }
    }

    /// The error of an operation refused, unrun, because the file takes no
    /// more changes (`why`, `Store::writes_refused`) or is hosted read-only:
    /// nothing of it is stored.
    fn refused(why: &str) -> Self {
        StorageError {
            code: crate::hosting::save_error(&std::io::Error::other(why)),
            message: why.to_owned(),
            unknown: false,
        }
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
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

    /// The outcome when what the operation did could not be stored, or may
    /// not have been (`error.unknown`): the storage error in place of its
    /// reply, and no notices (docs/server-storage.md 5, where the table of
    /// replies is). op70 fails with the save's message
    /// (`writes::CommitError::NotSaved`, which its caller answers with a
    /// system exception); op71, op72 and op88 with the save's FileMaker
    /// error in their status; op13, op73 and op86's schema instructions with
    /// the error their callers encode (a system exception for op13 and
    /// op73, op86's error slot); a find's indexes as not stored; op87 with
    /// its failure count; an authoring edit with the save's FileMaker error
    /// and a message saying what the file keeps, or, when the outcome is
    /// unknown, the extension's status for it; op240's Save as failed or
    /// unknown; a server script's commit and Set Next Serial Value with the
    /// save's FileMaker error, the commit's message saying the file could
    /// not be saved; an account edit as unwritten with the save's message,
    /// or as of an unknown outcome; a found set as not kept.
    fn not_stored(self, error: &StorageError) -> Outcome {
        let unwritten = || format!("the file could not be saved: {error}");
        let account_error = || {
            if error.unknown {
                AccountEditError::OutcomeUnknown(error.message.clone())
            } else {
                AccountEditError::Unwritten(unwritten())
            }
        };
        let reply = match self.reply {
            Reply::EditDesign(Edited { not_stored, reply }) => Reply::EditDesign(Edited {
                reply: if error.unknown {
                    design::Reply::Refused {
                        status: design::status::OUTCOME_UNKNOWN,
                        message: format!(
                            "The change may or may not be in the file: {error}. The file takes \
                             no more changes until it is hosted again."
                        ),
                        current: None,
                    }
                } else {
                    match not_stored {
                        Some(not_stored) => design::Reply::Refused {
                            status: error.code,
                            message: format!(
                                "The file could not be saved ({error}); {}.",
                                not_stored.kept
                            ),
                            current: None,
                        },
                        None => reply,
                    }
                },
                not_stored: None,
            }),
            Reply::ReserveRecordIds(_) => {
                Reply::ReserveRecordIds(writes::refused_ids(u32::from(error.code)))
            }
            Reply::AdvanceSerial(_) => Reply::AdvanceSerial(GetSerialReply {
                conflict: None,
                status: u32::from(error.code),
                value: String::new(),
            }),
            Reply::CommitRecords(reply) => Reply::CommitRecords(CommitReply {
                result: Err(CommitError::NotSaved(format!(
                    "commit of {} record(s) {}: {}",
                    self.notices.changes.len(),
                    if error.unknown {
                        "may or may not be stored"
                    } else {
                        "taken back"
                    },
                    unwritten()
                ))),
                unused: reply.unused,
            }),
            Reply::UploadToCatalog(_) => Reply::UploadToCatalog(Err(error.clone())),
            Reply::ChangeSchema(_) => {
                Reply::ChangeSchema(Err(SchemaNotChanged::NotStored(error.clone())))
            }
            Reply::StoreIndexes(_) => Reply::StoreIndexes(Err(error.clone())),
            Reply::UpdateTable(_) => Reply::UpdateTable(Err(unwritten())),
            Reply::StoreContainerData(_) => Reply::StoreContainerData(
                crate::containers::upload_reply(error.code, Vec::new(), unwritten()),
            ),
            Reply::AdjustContainerCount(_) => Reply::AdjustContainerCount(COUNT_FAILED),
            Reply::SaveFile(_) if error.unknown => {
                Reply::SaveFile(Err(FileNotSaved::Unknown(error.clone())))
            }
            Reply::SaveFile(_) => Reply::SaveFile(Err(FileNotSaved::Failed(error.clone()))),
            Reply::CommitScriptRecords(_) => {
                Reply::CommitScriptRecords(Err((u32::from(error.code), unwritten())))
            }
            Reply::SetNextSerial(_) => Reply::SetNextSerial(Err(u32::from(error.code))),
            Reply::ChangePassword(Ok(())) => Reply::ChangePassword(Err(if error.unknown {
                PasswordChangeError::OutcomeUnknown(error.message.clone())
            } else {
                PasswordChangeError::Unwritten(unwritten())
            })),
            Reply::ResetPassword(Ok(())) => Reply::ResetPassword(Err(account_error())),
            Reply::EnableAccount(Ok(_)) => Reply::EnableAccount(Err(account_error())),
            Reply::DeleteAccount(Ok(())) => Reply::DeleteAccount(Err(account_error())),
            Reply::SaveFoundSet(_) => Reply::SaveFoundSet(false),
            Reply::FinishDeferredWork(_) => Reply::FinishDeferredWork(Err(error.clone())),
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

/// The outcome of `operation`, refused without running because the file
/// takes no more changes (`error`, [`StorageError::refused`]): its storage
/// error, as [`Outcome::not_stored`] gives it for the reply it would have
/// had, nothing of it stored and no notices. op70's staged records go back
/// to the session's temporary area.
fn not_run(operation: Operation, error: &StorageError) -> Outcome {
    let unchanged = || NotStored {
        not_done: "not done".into(),
        kept: "nothing was changed".into(),
    };
    let reply = match operation {
        Operation::ReserveRecordIds(_) => Reply::ReserveRecordIds(writes::refused_ids(0)),
        Operation::AdvanceSerial(_) => Reply::AdvanceSerial(GetSerialReply {
            conflict: None,
            status: 0,
            value: String::new(),
        }),
        Operation::CommitRecords(op) => Reply::CommitRecords(CommitReply {
            result: Ok(()),
            unused: op.staged,
        }),
        Operation::UploadToCatalog(_) => Reply::UploadToCatalog(Ok(false)),
        Operation::ChangeSchema(_) => Reply::ChangeSchema(Ok(())),
        Operation::StoreIndexes(_) => Reply::StoreIndexes(Ok(())),
        Operation::UpdateTable(_) => Reply::UpdateTable(Ok(false)),
        Operation::StoreContainerData(_) => Reply::StoreContainerData(
            crate::containers::upload_reply(0, Vec::new(), String::new()),
        ),
        Operation::AdjustContainerCount(_) => Reply::AdjustContainerCount(0),
        Operation::EditDesign(_) => Reply::EditDesign(Edited {
            reply: design::Reply::Saved,
            not_stored: Some(unchanged()),
        }),
        // Whether the disk holds what the file holds is what is unknown.
        Operation::SaveFile => {
            let error = StorageError {
                unknown: true,
                ..error.clone()
            };
            return Outcome::new(Reply::SaveFile(Err(FileNotSaved::Unknown(error))));
        }
        Operation::CommitScriptRecords(_) => Reply::CommitScriptRecords(Ok(())),
        Operation::SetNextSerial(_) => Reply::SetNextSerial(Ok(())),
        Operation::ChangePassword(_) => Reply::ChangePassword(Ok(())),
        Operation::ResetPassword(_) => Reply::ResetPassword(Ok(())),
        Operation::EnableAccount(_) => Reply::EnableAccount(Ok(false)),
        Operation::DeleteAccount(_) => Reply::DeleteAccount(Ok(())),
        Operation::FinishDeferredWork => Reply::FinishDeferredWork(Ok(false)),
        Operation::SaveFoundSet(_) => Reply::SaveFoundSet(false),
        // Not refused: they change only the lock table, or read.
        other => return refusal(other, NOT_HELD),
    };
    Outcome::new(reply).not_stored(error)
}

/// What an operation may read of the world besides the file: the clock and
/// a random source, so that two engines can be given the same seeded source
/// (docs/server-storage.md 12). [`run`] reads them only through this, and
/// so do the formulas an operation evaluates (a privilege set's record
/// limit, `writes::refused_action`; the stored calculations op73 and the
/// deferred work finished at opening recompute, `Store::update_table`):
/// their record environment ([`crate::recalc::record_environment_in`])
/// takes from it one clock reading (the date and time `Get` selectors, and
/// today for a year text leaves out), a UUID, the seed of `Random`'s
/// generator, and the stopwatch of a record limit's time budget. So do an
/// authoring edit's stamps and the UUIDs of what it adds, an account
/// edit's stamps, status time and password salt, and the unique ids of the
/// container data op73's recalculation, op88 and a server script's commit
/// store; the order of the draws is in the module doc ("The environment").
/// Functions a record limit may call still read the process
/// ([#702](https://github.com/OpenMaker-Dev/openmaker/issues/702)):
/// `CryptEncrypt` and `CryptEncryptBase64` draw their salt from the
/// system, so a limit comparing their output need not replay; the size and
/// attributes of a PDF a formula makes (`GetWidth ( Base64Decode ( ... ;
/// "a.pdf" ) )`) come from a worker with a wall-clock deadline, so such a
/// limit can fail on a slow machine; `CryptGenerateSignature` blinds with
/// the system's randomness, which its result does not depend on.
pub trait Environment: Send + Sync {
    /// The host's time now, in its time zone.
    fn now(&self) -> jiff::Zoned;
    /// Fill `bytes` from the random source, which does not fail.
    fn fill_random(&self, bytes: &mut [u8]);
    /// A stopwatch started now: each reading answers the time since, never
    /// less than the reading before. The time budgets of an operation's
    /// formulas are measured on it, the evaluator reading it each time it
    /// checks whether to stop. The host's answers the monotonic time that
    /// elapsed. A seeded environment's answers virtual time that advances
    /// by a fixed step at each reading, so that a budget is met after a
    /// fixed number of the evaluator's checks, the same on every machine;
    /// one that never advances would leave a formula with a large
    /// `SetRecursion` allowance unbounded.
    fn stopwatch(&self) -> om_data::rules::Stopwatch;
    /// Fill `bytes` from the random source, or say that it failed: for a
    /// draw whose failure is a reply, as a password's salt is ("no random
    /// salt"). By default [`Self::fill_random`].
    fn try_fill_random(&self, bytes: &mut [u8]) -> Result<(), NoRandom> {
        self.fill_random(bytes);
        Ok(())
    }
}

/// The random source gave no bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoRandom;

/// What only the engine can make, its field being private to `engine`:
/// the file's store for writing and its lock table for changing take one
/// (`Hosted::write_store`, `try_write_store`, `locks`), so that code
/// outside the engine that would change a hosted file does not compile
/// (docs/server-storage.md 5). A `Store` can be changed only through the
/// write guard, and a lock table read through `Hosted::lock_table` only
/// read.
pub(crate) struct Writer(());

/// The engine's [`Writer`].
const WRITER: Writer = Writer(());

impl Writer {
    /// One for a test that sets a file up, or breaks it, on purpose.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Writer(())
    }
}

/// The host's clock and the system's random source.
#[derive(Debug, Default)]
pub struct HostEnvironment;

impl Environment for HostEnvironment {
    fn now(&self) -> jiff::Zoned {
        jiff::Zoned::now()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        self.try_fill_random(bytes)
            .expect("the system random source is available");
    }

    fn try_fill_random(&self, bytes: &mut [u8]) -> Result<(), NoRandom> {
        use ring::rand::SecureRandom as _;
        ring::rand::SystemRandom::new()
            .fill(bytes)
            .map_err(|_| NoRandom)
    }

    fn stopwatch(&self) -> om_data::rules::Stopwatch {
        let started = std::time::Instant::now();
        std::sync::Arc::new(move || started.elapsed())
    }
}

/// A random UUID in `Get ( UUID )`'s form, from `environment`.
pub fn uuid(environment: &dyn Environment) -> String {
    let mut bytes = [0u8; 16];
    environment.fill_random(&mut bytes);
    crate::recalc::uuid_text(bytes)
}

/// Run `operation` on the file named `file` (for log lines), with the
/// passwords it was given beside it (`credentials`, the module doc's
/// "Credentials"), against what the engine holds of it (`hold`) and its
/// lock table (`locks`), taking the clock and randomness from
/// `environment`, its formulas' too ([`Environment`]). It sends nothing,
/// saves nothing and takes no lock of its own: the effects are what it did
/// to the transaction and the lock table, and the engine stores them
/// before the outcome may go out. A refusal can keep effects: op71 refused
/// for a lock another user holds has used up its ids (write-path.md 3.1).
pub fn run(
    operation: Operation,
    credentials: Credentials,
    file: &str,
    hold: Hold<'_, '_>,
    locks: &mut Locks<'_>,
    environment: &dyn Environment,
) -> Outcome {
    let takes_credentials = matches!(
        operation,
        Operation::ChangePassword(_) | Operation::ResetPassword(_)
    );
    if !takes_credentials && !matches!(credentials, Credentials::None) {
        return unfit(operation, file);
    }
    match (operation, hold) {
        (Operation::ReserveRecordIds(op), Hold::Transaction(tx)) => Outcome::new(
            Reply::ReserveRecordIds(writes::create_record(op, file, tx, locks)),
        ),
        (Operation::AdvanceSerial(op), Hold::Transaction(tx)) => Outcome::new(
            Reply::AdvanceSerial(writes::get_serial(&op.request, file, tx)),
        ),
        (Operation::CommitRecords(op), Hold::Transaction(tx)) => {
            let from = op.user.connection;
            let (reply, changes) = writes::commit(op, file, tx, locks, environment);
            Outcome {
                reply: Reply::CommitRecords(reply),
                notices: Notices { from, changes },
            }
        }
        (Operation::TakeLock(op), Hold::View(store)) => Outcome::new(Reply::TakeLock(
            writes::lock(op, file, &store.tree_view(), locks),
        )),
        (Operation::TakeLock(op), Hold::Transaction(tx)) => {
            Outcome::new(Reply::TakeLock(writes::lock(op, file, tx, locks)))
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
            Outcome::new(Reply::UploadToCatalog(
                schema::upload_to_catalog(op, file, tx)
                    .map(|()| true)
                    .map_err(|why| StorageError::refused(&why)),
            ))
        }
        (Operation::ChangeSchema(op), Hold::Transaction(tx)) => Outcome::new(Reply::ChangeSchema(
            schema::change_schema(op, file, tx).map_err(SchemaNotChanged::Refused),
        )),
        (Operation::StoreIndexes(op), Hold::Transaction(tx)) => {
            tx.store_indexes(op.indexes);
            Outcome::new(Reply::StoreIndexes(Ok(())))
        }
        (Operation::UpdateTable(op), Hold::Transaction(tx)) => Outcome::new(Reply::UpdateTable(
            schema::update_table(op, tx, environment).map_err(String::from),
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
        (Operation::CommitScriptRecords(op), Hold::Transaction(tx)) => {
            let from = op.user.connection;
            let (result, changes) = writes::commit_records(op, file, tx, locks, environment);
            Outcome {
                reply: Reply::CommitScriptRecords(result),
                notices: Notices { from, changes },
            }
        }
        (Operation::SetNextSerial(op), Hold::Transaction(tx)) => {
            Outcome::new(Reply::SetNextSerial(writes::set_next_serial(op, file, tx)))
        }
        (Operation::TakeScriptLock(op), _) => Outcome::new(Reply::TakeScriptLock(
            locks.lock(&op.path, op.user, LOCK_WRITE, &op.holder),
        )),
        (Operation::ReleaseScriptLock(op), _) => {
            let ReleaseScriptLock {
                user,
                path,
                changes,
            } = op;
            locks.release_path(&path, user);
            Outcome {
                reply: Reply::ReleaseScriptLock,
                notices: Notices {
                    from: user.connection,
                    changes,
                },
            }
        }
        (Operation::ReleaseSessionLocks(op), _) => {
            locks.release_connection(op.connection);
            Outcome::new(Reply::ReleaseSessionLocks)
        }
        (Operation::ChangePassword(op), Hold::Transaction(tx)) => {
            let Credentials::Change { old, new } = credentials else {
                return unfit(Operation::ChangePassword(op), file);
            };
            let reply = crate::hosting::change_password(op, old, new, tx, environment);
            Outcome::new(Reply::ChangePassword(reply))
        }
        (Operation::ResetPassword(op), Hold::Transaction(tx)) => {
            let Credentials::Reset { new } = credentials else {
                return unfit(Operation::ResetPassword(op), file);
            };
            let reply = crate::hosting::reset_account_password(op, new, tx, environment);
            Outcome::new(Reply::ResetPassword(reply))
        }
        (Operation::EnableAccount(op), Hold::Transaction(tx)) => Outcome::new(
            Reply::EnableAccount(crate::hosting::enable_account(op, tx, environment)),
        ),
        (Operation::DeleteAccount(op), Hold::Transaction(tx)) => Outcome::new(
            Reply::DeleteAccount(crate::hosting::delete_account(op, tx, environment)),
        ),
        (Operation::FinishDeferredWork, Hold::Transaction(tx)) => Outcome::new(
            Reply::FinishDeferredWork(Ok(schema::finish_deferred_work(tx, environment))),
        ),
        (Operation::SaveFoundSet(op), Hold::Transaction(tx)) => {
            Outcome::new(Reply::SaveFoundSet(save_found_set(op, file, tx)))
        }
        (operation, _) => not_held(operation, file),
    }
}

/// Keep the window's found set `op` in the file named `file` (for log
/// lines), within the transaction's commit: whether the file holds it. A
/// read-only file, a table without a data version and a node FileMaker
/// Pro never wrote that way write nothing; a node that already holds it
/// is left as it is.
fn save_found_set(op: SaveFoundSet, file: &str, tx: &mut Transaction<'_>) -> bool {
    let SaveFoundSet {
        occurrence,
        table,
        records,
        current,
    } = op;
    if tx.is_read_only() {
        return false;
    }
    let version = View::node(tx, &[&[3], &[0x10], &[5], &table, &[0xfb]])
        .ok()
        .flatten()
        .and_then(|head| head.value)
        .and_then(|value| <[u8; 4]>::try_from(&*value).ok())
        .map(u32::from_be_bytes);
    let Some(data_version) = version else {
        log::info!("{file}: no data version for table {table:02x?}; found set not kept");
        return false;
    };
    let write = om_data::meta::FoundSetWrite {
        records,
        current,
        data_version,
    };
    // The saved found set whole, as FileMaker Pro writes it.
    let existing = tx
        .subtree(&[&occurrence, &[7]], NO_LIMIT)
        .ok()
        .flatten()
        .map(Cow::into_owned);
    let Some(node) = om_data::meta::saved_found_set_written(existing.as_ref(), &table, &write)
    else {
        log::info!(
            "{file}: occurrence {occurrence:02x?} holds a found set FileMaker Pro never wrote \
             that way; not kept"
        );
        return false;
    };
    if existing.as_ref() == Some(&node) {
        return true;
    }
    tx.begin_commit();
    if let Err(reason) = tx.replace(vec![occurrence, vec![7]], node) {
        tx.refuse(reason);
    }
    true
}

/// The refusal of an operation run without the hold of the file it needs:
/// an engine's mistake, refused with FileMaker error 3 and logged.
fn not_held(operation: Operation, file: &str) -> Outcome {
    error!("{file:?}: an operation ran without the hold of the file it needs: {operation:?}");
    refusal(operation, NOT_HELD)
}

/// The refusal of an operation submitted without the passwords it takes,
/// or with passwords it does not take: a caller's mistake, refused as
/// [`not_held`] is and logged (without the passwords).
fn unfit(operation: Operation, file: &str) -> Outcome {
    error!("{file:?}: an operation came without the passwords it takes, or with some it does not: {operation:?}");
    refusal(operation, UNFIT)
}

/// `operation`'s refusal, with FileMaker error 3 where its reply has one
/// and `why` where it has a message.
fn refusal(operation: Operation, why: &'static str) -> Outcome {
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
        Operation::UploadToCatalog(_) => Reply::UploadToCatalog(Err(StorageError::refused(why))),
        Operation::ChangeSchema(_) => Reply::ChangeSchema(Err(SchemaNotChanged::Refused(why))),
        Operation::StoreIndexes(_) => Reply::StoreIndexes(Ok(())),
        Operation::UpdateTable(_) => Reply::UpdateTable(Err(why.to_owned())),
        Operation::StoreContainerData(_) => Reply::StoreContainerData(
            crate::containers::upload_reply(writes::UNAVAILABLE as u16, Vec::new(), why.into()),
        ),
        Operation::AdjustContainerCount(_) => Reply::AdjustContainerCount(COUNT_FAILED),
        Operation::EditDesign(_) => Reply::EditDesign(Edited {
            reply: design::Reply::Refused {
                status: design::status::UNSUPPORTED,
                message: why.into(),
                current: None,
            },
            not_stored: None,
        }),
        Operation::SaveFile => Reply::SaveFile(Err(FileNotSaved::Failed(StorageError {
            code: writes::UNAVAILABLE as u16,
            message: why.into(),
            unknown: false,
        }))),
        Operation::CommitScriptRecords(_) => {
            Reply::CommitScriptRecords(Err((writes::UNAVAILABLE, why.into())))
        }
        Operation::SetNextSerial(_) => Reply::SetNextSerial(Err(writes::UNAVAILABLE)),
        Operation::TakeScriptLock(_) => Reply::TakeScriptLock(Err(Conflict {
            self_conflict: false,
            users: String::new(),
        })),
        Operation::ReleaseScriptLock(_) => Reply::ReleaseScriptLock,
        Operation::ReleaseSessionLocks(_) => Reply::ReleaseSessionLocks,
        Operation::ChangePassword(_) => {
            Reply::ChangePassword(Err(PasswordChangeError::Unwritten(why.into())))
        }
        Operation::ResetPassword(_) => {
            Reply::ResetPassword(Err(AccountEditError::Unwritten(why.into())))
        }
        Operation::EnableAccount(_) => {
            Reply::EnableAccount(Err(AccountEditError::Unwritten(why.into())))
        }
        Operation::DeleteAccount(_) => {
            Reply::DeleteAccount(Err(AccountEditError::Unwritten(why.into())))
        }
        Operation::FinishDeferredWork => Reply::FinishDeferredWork(Ok(false)),
        Operation::SaveFoundSet(_) => Reply::SaveFoundSet(false),
    })
}

const NOT_HELD: &str = "the engine did not hold the file";
const UNFIT: &str = "the operation came without the passwords it takes";
