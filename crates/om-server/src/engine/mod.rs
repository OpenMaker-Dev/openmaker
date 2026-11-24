//! The engine interface (`docs/server-storage.md` 5) for record writes and
//! locks: every change of this kind a request makes is an [`Operation`], a
//! value that owns its data, [`run`] against what the engine holds of the
//! file, and answered with an [`Outcome`]: the reply and the notices for
//! other clients. Effects are what the run did to the [`Transaction`] and
//! the [`Locks`]. The file engine ([`FileEngine`]) stores them, and only
//! then does the caller encode the reply and deliver the notices.
//!
//! Operations so far: commit records (op70), reserve record ids (op71),
//! advance a serial (op72), and take, upgrade and release locks (op60,
//! op61, op62, op65). The other writes (op13, op73, op86, op87, op88,
//! op240, accounts, a server script's commit) still change the file from
//! their handlers (#673).
//!
//! # Lock order
//!
//! [`FileEngine::submit`] takes, in this order and only what the operation
//! needs: the file's store (`Hosted::read_store` for op60,
//! `Hosted::write_store` for op70, op71 and op72), then its lock table
//! (`Hosted::locks`, when the operation first uses it, [`Locks`]). It holds
//! both while it saves, and lets go of the lock table first; dropping the
//! store's write guard takes the file's accounts while it still holds the
//! store (`hosting::StoreWriteGuard`). op61, op62 and op65 take only the
//! lock table, as before. Nothing else is taken inside `submit`: the
//! caller delivers notices after it returns, taking the file's
//! registrations and then the mailboxes (`writes::notify`), and a
//! session's own state (its temporary area, the records it was served
//! withheld) is read before and settled after. This is the order the write
//! path has always used (store, lock table, registrations, mailboxes),
//! with the last two no longer inside the first two.

mod file;
mod transaction;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use log::error;
use om_data::host::security::SetAccess;
use om_hbam::OwnedNode;
use om_protocol::schema::Upgrade;
use om_protocol::write::{
    Conflict, CreateRecord, CreateRecordReply, GetSerial, GetSerialReply, KeyPath, KillAllLocks,
    Lock, LockReply, Unlock,
};

use crate::hosting::Hosted;
use crate::locks::LockUser;
use crate::views::Withheld;
use crate::writes::{self, Change, CommitError, Mailboxes};

pub(crate) use file::FileEngine;
pub(crate) use transaction::{Hold, Locks, Transaction};

/// A change the server makes to a hosted file: one request's work, as a
/// value. Who asks is part of it (the lock user, the name other users see
/// for it, the account and privilege set a commit is checked against), so
/// that it can be run by another engine, recorded with its outcome and
/// replayed (docs/server-storage.md 5, 7.2, 7.3, 12).
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// (`writes::CommitError::NotSaved`), op71 with the save's FileMaker
    /// error ([`crate::hosting::save_error`]).
    fn not_stored(self, error: &std::io::Error) -> Outcome {
        let reply = match self.reply {
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
/// a random source. [`run`] reads them only through this, so that two
/// engines can be given the same seeded source (docs/server-storage.md 12).
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
/// the engine holds of it (`hold`) and its lock table (`locks`), reading
/// the clock and randomness only from `environment`. It sends nothing,
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
    })
}
