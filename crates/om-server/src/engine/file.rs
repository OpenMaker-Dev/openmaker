//! The file engine: each hosted file one `.fmp12`, loaded whole into a
//! `Store` and saved whole (docs/server-storage.md 1, 5).

use log::{info, warn};

use super::transaction::{self, Hold, Locks, Transaction};
use super::{
    not_run, run, ChangeSchema, Credentials, Draft, Edited, Environment, FileNotSaved,
    HostEnvironment, Operation, Outcome, Reply, StorageError, StoreIndexes, WRITER,
};
use crate::hosting::{Hosted, Saving, StoreWriteGuard};
use crate::store::{IndexId, MemberKey, SaveError, Store};

/// The file engine. Its environment is the one operations read the clock
/// and randomness from ([`Environment`]).
pub struct FileEngine {
    environment: Box<dyn Environment>,
    recorder: Option<Recorder>,
    /// The test view reads go through when a test asked for it
    /// (`super::copies`).
    #[cfg(debug_assertions)]
    pub(super) copies: std::sync::RwLock<Option<std::sync::Arc<super::copies::Switch>>>,
}

/// What the engine offers each operation to, by reference, completed and
/// before it runs it: a recording (`engine::tests`). The running server
/// installs none, so the engine keeps no copy of what an operation owns.
/// An account edit's passwords are not part of its operation
/// ([`Credentials`]), so a recording never gets them.
type Recorder = Box<dyn Fn(&Operation) + Send + Sync>;

impl Default for FileEngine {
    fn default() -> Self {
        Self::new(Box::new(HostEnvironment))
    }
}

/// What of the file the engine holds for an operation: what today's code
/// took for each request. op61, op62 and op65 never took the store, nor did
/// a server script's lock and its releases or a session's end, and op60
/// only read it. op13, op73, op86's writes, a find's indexes, op87, op88,
/// op240's save (and a server script's end), Set Next Serial Value, the
/// account edits, the deferred field work finished at opening and a saved
/// found set took the store for writing and never the lock table, which
/// their runs do not use (a found set only when no one held the store,
/// [`FileEngine::try_submit`]); an authoring edit took the store for
/// writing and the lock table only while it took and released its locks;
/// a server script's commit took both from its start.
enum Needs {
    LockTable,
    View,
    Transaction,
}

/// When the engine saves what an operation changed: as each request saved
/// before the engine interface, so that with saves succeeding the file is
/// written as often, and at the same points, as before. Every save is one
/// ([`Hosted::save_change`]), and its result is read: what the operation
/// changed is stored, taken back, or kept with the file taking no more
/// changes ([`FileEngine::submit`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Save {
    /// Nothing to save: lock-table edits only, or a server script's op71,
    /// whose reservation its commit saves, or its end
    /// ([`Save::Requested`]). What it changed stays in memory, unsaved.
    Nothing,
    /// The operation's commit, when it began one: op70, an authoring edit,
    /// a server script's commit and Set Next Serial Value, the account
    /// edits.
    Commit,
    /// A window's saved found set: its commit, saved as the desktop always
    /// saved it, so a failed save raises no alert and logs only the
    /// engine's warning (an unknown outcome is alerted all the same).
    QuietCommit,
    /// A client's op71: its reservation is saved before the ids are
    /// answered (#927); on a read-only file the save fails.
    Reservation,
    /// op72, op13, op86's writes, a find's indexes, op87 and op88: the file
    /// is saved if it holds changes the disk lacks, unless it is hosted
    /// read-only.
    Always,
    /// op73 and the deferred field work finished at opening:
    /// [`Save::Always`] when the work changed the file, which is otherwise
    /// not saved.
    IfChanged,
    /// op240's save, when the file may be written: the reply then says
    /// whether the disk holds every change the file holds. A server
    /// script's end submits it too.
    Requested,
}

fn needs(operation: &Operation) -> Needs {
    match operation {
        Operation::ReserveRecordIds(_)
        | Operation::AdvanceSerial(_)
        | Operation::CommitRecords(_)
        | Operation::UploadToCatalog(_)
        | Operation::ChangeSchema(_)
        | Operation::StoreIndexes(_)
        | Operation::UpdateTable(_)
        | Operation::StoreContainerData(_)
        | Operation::AdjustContainerCount(_)
        | Operation::EditDesign(_)
        | Operation::SaveFile
        | Operation::CommitScriptRecords(_)
        | Operation::SetNextSerial(_)
        | Operation::ChangePassword(_)
        | Operation::ResetPassword(_)
        | Operation::EnableAccount(_)
        | Operation::DeleteAccount(_)
        | Operation::FinishDeferredWork
        | Operation::SaveFoundSet(_) => Needs::Transaction,
        Operation::TakeLock(_) => Needs::View,
        Operation::UpgradeLock(_)
        | Operation::ReleaseLock(_)
        | Operation::ReleaseLocks(_)
        | Operation::TakeScriptLock(_)
        | Operation::ReleaseScriptLock(_)
        | Operation::ReleaseSessionLocks(_) => Needs::LockTable,
    }
}

fn save(operation: &Operation) -> Save {
    match operation {
        Operation::ReserveRecordIds(op) if op.durable => Save::Reservation,
        // An authoring edit or an account edit that changed the file did
        // it in a commit, and so does a server script's Set Next Serial
        // Value.
        Operation::CommitRecords(_)
        | Operation::EditDesign(_)
        | Operation::CommitScriptRecords(_)
        | Operation::SetNextSerial(_)
        | Operation::ChangePassword(_)
        | Operation::ResetPassword(_)
        | Operation::EnableAccount(_)
        | Operation::DeleteAccount(_) => Save::Commit,
        Operation::SaveFoundSet(_) => Save::QuietCommit,
        Operation::AdvanceSerial(_)
        | Operation::UploadToCatalog(_)
        | Operation::ChangeSchema(_)
        | Operation::StoreIndexes(_)
        | Operation::StoreContainerData(_)
        | Operation::AdjustContainerCount(_) => Save::Always,
        Operation::UpdateTable(_) | Operation::FinishDeferredWork => Save::IfChanged,
        Operation::SaveFile => Save::Requested,
        _ => Save::Nothing,
    }
}

/// Whether `operation` would change the file, as its value says before it
/// runs: on a file that takes no more changes, only these are refused
/// (docs/server-storage.md 5). op72 that neither replaces nor advances the
/// serial (flags 0) and op87 with a delta of 0 read, and so does a find's
/// draft with no index to store; every other operation that takes the store
/// for writing may change it.
fn changes_file(operation: &Operation) -> bool {
    use om_protocol::write::{SERIAL_INCREMENT, SERIAL_REPLACE};
    match operation {
        Operation::AdvanceSerial(op) => op.request.flags & (SERIAL_REPLACE | SERIAL_INCREMENT) != 0,
        Operation::AdjustContainerCount(op) => op.request.delta != 0,
        Operation::StoreIndexes(op) => !op.indexes.is_empty(),
        _ => true,
    }
}

/// What a completed draft took of what reads left waiting: the member keys
/// and indexes, small values, which wait again when the operation is not
/// stored or its run is left by a panic.
#[derive(Default)]
struct Taken {
    member_keys: Vec<MemberKey>,
    indexes: Vec<IndexId>,
}

impl Taken {
    /// Put it back among what waits in `store`.
    fn put_back(self, store: &mut Store) {
        store.put_back_member_keys(self.member_keys);
        store.put_back_indexes(self.indexes);
    }
}

/// Complete `draft` with what reads left waiting in `store`, which the
/// engine holds for writing (the module doc of [`super`], "Drafts"): all of
/// it, as the first write to hold the store took it before the engine
/// interface, and only what the run will store. Writes refused on a
/// read-only file take nothing, and member keys stay waiting on a
/// read-only file. Also returns what it took ([`Taken`]).
fn complete(draft: Draft, store: &mut Store) -> (Operation, Taken) {
    match draft {
        Draft::ChangeSchema { writes } => {
            if store.is_read_only() && !writes.is_empty() {
                let refused = Operation::ChangeSchema(ChangeSchema {
                    writes,
                    member_keys: Vec::new(),
                    indexes: Vec::new(),
                });
                return (refused, Taken::default());
            }
            let indexes = store.take_indexes_on_demand();
            let member_keys = if store.is_read_only() {
                Vec::new()
            } else {
                store.take_issued_member_keys()
            };
            let taken = Taken {
                member_keys: member_keys.clone(),
                indexes: indexes.clone(),
            };
            let operation = if writes.is_empty() && member_keys.is_empty() {
                Operation::StoreIndexes(StoreIndexes { indexes })
            } else {
                Operation::ChangeSchema(ChangeSchema {
                    writes,
                    member_keys,
                    indexes,
                })
            };
            (operation, taken)
        }
        Draft::StoreIndexes => {
            let indexes = store.take_indexes_on_demand();
            let taken = Taken {
                member_keys: Vec::new(),
                indexes: indexes.clone(),
            };
            (Operation::StoreIndexes(StoreIndexes { indexes }), taken)
        }
    }
}

impl FileEngine {
    pub fn new(environment: Box<dyn Environment>) -> Self {
        FileEngine {
            environment,
            recorder: None,
            #[cfg(debug_assertions)]
            copies: Default::default(),
        }
    }

    /// This engine, offering each operation it runs to `recorder` first
    /// ([`Recorder`]).
    #[cfg(test)]
    pub fn recording(mut self, recorder: impl Fn(&Operation) + Send + Sync + 'static) -> Self {
        self.recorder = Some(Box::new(recorder));
        self
    }

    fn record(&self, operation: &Operation) {
        if let Some(recorder) = &self.recorder {
            recorder(operation);
        }
    }

    /// Run `operation` on `file` and store what it changed before
    /// returning its outcome, which the caller then sends: the reply
    /// encoded, the notices delivered. Operations on one file run one at a
    /// time under the store's write lock, or the lock table's mutex for
    /// those that only edit it (lock order: the module doc of
    /// [`super`]). Each save has one of three outcomes
    /// (docs/server-storage.md 2, rule 4; 5):
    ///
    /// - stored: the outcome as the operation gave it;
    /// - not stored (the save stopped before its rename): everything the
    ///   operation changed is taken back from its journal (the tree, the
    ///   schema read from it, op73's basis), so are the locks it took and
    ///   the member keys and indexes it took from what reads left waiting,
    ///   the releases and the log lines that waited for it are dropped, and
    ///   the outcome is its storage error with no notices
    ///   ([`Outcome::not_stored`]);
    /// - unknown (the folder flush after the rename failed): what it
    ///   changed is kept, as the renamed file holds it, the outcome is its
    ///   storage error saying so with no notices, and the file takes no
    ///   more changes until it is hosted again: every later operation that
    ///   would change it is refused unrun with the storage error, while
    ///   reads and lock requests go on.
    ///
    /// An operation that changed nothing of the tree (a serial read, a
    /// delta of 0) is answered as it was when the save it makes, of
    /// changes others left waiting, fails; those changes wait on, and op240's
    /// Save says whether the disk holds them.
    pub fn submit(&self, file: &Hosted, operation: Operation) -> Outcome {
        self.submit_with(file, operation, Credentials::None)
    }

    /// [`Self::submit`] of an account edit, with the passwords it was given
    /// beside it: Change Password's and Reset Account Password's
    /// ([`Credentials`]). They reach the run and nothing else: not the
    /// recording, which gets the operation alone.
    pub fn submit_with(
        &self,
        file: &Hosted,
        operation: Operation,
        credentials: Credentials,
    ) -> Outcome {
        let environment = &*self.environment;
        match needs(&operation) {
            Needs::LockTable => {
                self.record(&operation);
                let mut locks = Locks::new(file.locks(&WRITER));
                let outcome = run(
                    operation,
                    credentials,
                    &file.name,
                    Hold::LockTable,
                    &mut locks,
                    environment,
                );
                locks.keep();
                outcome
            }
            Needs::View => {
                let store = file.read_store();
                self.record(&operation);
                let mut locks = Locks::new(file.locks(&WRITER));
                let outcome = run(
                    operation,
                    credentials,
                    &file.name,
                    Hold::View(&store),
                    &mut locks,
                    environment,
                );
                locks.keep();
                outcome
            }
            Needs::Transaction => {
                self.submit_write(file, file.write_store(&WRITER), credentials, |_| {
                    (operation, Taken::default())
                })
            }
        }
    }

    /// [`Self::submit`], for an operation that takes the store for
    /// writing, only if no one holds the store now: `None` when someone
    /// does, and then nothing ran, nothing was recorded and the operation
    /// is dropped. For work that must not wait on a request: a window
    /// closing on a local file keeps its found set this way
    /// (`LocalHost::save_found_set`).
    pub fn try_submit(&self, file: &Hosted, operation: Operation) -> Option<Outcome> {
        match needs(&operation) {
            Needs::Transaction => {
                let store = file.try_write_store(&WRITER)?;
                Some(self.submit_write(file, store, Credentials::None, |_| {
                    (operation, Taken::default())
                }))
            }
            Needs::LockTable | Needs::View => Some(self.submit(file, operation)),
        }
    }

    /// Complete `draft` with what reads left waiting in `file`, under the
    /// store's write lock, and run the operation it becomes as
    /// [`Self::submit`] does, in the same hold. A recording gets the
    /// completed operation ([`Self::recording`]), which [`Self::submit`]
    /// replays.
    pub fn submit_draft(&self, file: &Hosted, draft: Draft) -> Outcome {
        self.submit_write(
            file,
            file.write_store(&WRITER),
            Credentials::None,
            |store| complete(draft, store),
        )
    }

    /// Run the operation `operation` gives for `store`, `file`'s, which
    /// the caller took for writing, with what it took of what reads left
    /// waiting ([`complete`]), and store it ([`Self::submit`]). On a file
    /// that takes no more changes the operation is refused unrun, and what
    /// it took waits again. If the run is left by a panic, what it changed
    /// is taken back and the member keys it took wait again.
    fn submit_write(
        &self,
        file: &Hosted,
        mut store: StoreWriteGuard<'_>,
        credentials: Credentials,
        operation: impl FnOnce(&mut Store) -> (Operation, Taken),
    ) -> Outcome {
        let (operation, taken) = operation(&mut store);
        if let Some(why) = store.writes_refused().filter(|_| changes_file(&operation)) {
            let error = StorageError::refused(why);
            taken.put_back(&mut store);
            warn!("{:?}: not done: {error}", file.name);
            return not_run(operation, &error);
        }
        self.record(&operation);
        let save = save(&operation);
        let mut locks = Locks::new(file.locks(&WRITER));
        let mut tx = Transaction::new(&mut store);
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(
                operation,
                credentials,
                &file.name,
                Hold::Transaction(&mut tx),
                &mut locks,
                &*self.environment,
            )
        }));
        let mut outcome = match ran {
            Ok(outcome) => outcome,
            Err(panic) => {
                // What it changed is put back, member keys it wrote
                // included, which wait again, as they did before the engine
                // interface; the indexes it took do not (they had been
                // taken then too).
                let journal = tx.journal;
                store.roll_back(journal);
                store.put_back_member_keys(taken.member_keys);
                std::panic::resume_unwind(panic)
            }
        };
        // An authoring edit's locks, released once the store is let go.
        let held = locks.after_stored();
        let Transaction {
            mut journal,
            committing,
            reserved,
            stored_lines,
            released_libraries,
            emptied_libraries,
            field_bases,
            ..
        } = tx;
        // The accounts follow the change in the same step, before its save
        // (an account edit's, an op13 upload of the security catalog's):
        // a save that is not written puts back those from before with it.
        store.follow_accounts(Some(&mut journal));
        let effects = journal.changed();
        let lines = journal.take_lines();
        let saves = match save {
            Save::Nothing => false,
            Save::Commit | Save::QuietCommit => committing,
            Save::Reservation => reserved,
            Save::Always => !store.is_read_only(),
            Save::IfChanged => {
                matches!(
                    outcome.reply,
                    Reply::UpdateTable(Ok(true)) | Reply::FinishDeferredWork(Ok(true))
                ) && !store.is_read_only()
            }
            Save::Requested => outcome.reply == Reply::SaveFile(Ok(())),
        };
        let saved = if !saves {
            // Kept in memory, unsaved: lock-table edits, what waits for a
            // later save, what a read-only file holds in memory.
            drop(journal);
            Ok(())
        } else if save == Save::Reservation && store.is_read_only() {
            store.roll_back(journal);
            Err(SaveError::NotWritten(std::io::Error::new(
                std::io::ErrorKind::ReadOnlyFilesystem,
                "the file is hosted read-only",
            )))
        } else {
            let saving = match save {
                Save::Commit => Saving::Commit,
                Save::QuietCommit => Saving::QuietCommit,
                _ => Saving::Change,
            };
            file.save_change(&mut store, journal, saving)
        };
        let outcome = match saved {
            Ok(()) => {
                stored(file, &released_libraries, &emptied_libraries);
                locks.keep();
                for (table, field) in field_bases {
                    store.add_field_basis(&table, field);
                }
                for line in stored_lines.into_iter().chain(lines) {
                    info!("{line}");
                }
                outcome
            }
            Err(error) if error.is_unknown() => {
                // The renamed file holds the change: the tree keeps it, as
                // what was derived from it does, and the file takes no
                // more changes (`Hosted::save_change`).
                stored(file, &released_libraries, &emptied_libraries);
                locks.keep();
                for (table, field) in field_bases {
                    store.add_field_basis(&table, field);
                }
                for line in stored_lines.into_iter().chain(lines) {
                    info!("{line} (not confirmed on disk)");
                }
                let error = StorageError::of(&error);
                if effects || outcome.reply == Reply::SaveFile(Ok(())) {
                    outcome.not_stored(&error)
                } else {
                    outcome
                }
            }
            Err(error) => {
                let error = StorageError::of(&error);
                if !effects {
                    // Nothing of its own to take back: what others left
                    // waiting waits on (`Hosted::save_change` alerted it),
                    // and what it took waits again.
                    taken.put_back(&mut store);
                    locks.keep();
                    if outcome.reply == Reply::SaveFile(Ok(())) {
                        outcome.reply = Reply::SaveFile(Err(FileNotSaved::Failed(error)));
                    }
                    outcome
                } else {
                    // The journal was put back (`Store::save_commit`).
                    locks.take_back();
                    taken.put_back(&mut store);
                    if let Reply::EditDesign(Edited {
                        not_stored: Some(not_stored),
                        ..
                    }) = &outcome.reply
                    {
                        warn!("{:?}: {}: {error}", file.name, not_stored.not_done);
                    }
                    if outcome.reply == Reply::SaveFoundSet(true) {
                        warn!(
                            "{}: keeping the window's found set failed, taken back: {error}",
                            file.name
                        );
                    }
                    // The work list is back, and the file cannot hold the
                    // work finished: served read-only, as when the work
                    // cannot be done (`Store::recover`).
                    if outcome.reply == Reply::FinishDeferredWork(Ok(true)) {
                        store.set_read_only(crate::store::ReadOnlyCause::DeferredWork);
                    }
                    outcome.not_stored(&error)
                }
            }
        };
        drop(store);
        transaction::release(file.locks(&WRITER), held);
        outcome
    }
}

/// What follows from an operation's change once the file holds it (stored,
/// or kept with an unknown outcome): the thumbnails of the libraries a
/// deleted table took, forgotten under the store, and those of libraries a
/// server script's commit emptied, whose member ids start again, forgotten
/// under the store and the lock table, as a server script's commit always
/// forgot them.
fn stored(file: &Hosted, released: &[Vec<Vec<u8>>], emptied: &[Vec<Vec<u8>>]) {
    if released.is_empty() && emptied.is_empty() {
        return;
    }
    let mut thumbnails = file.thumbnails.lock().expect("thumbnail lock");
    for library in released.iter().chain(emptied) {
        thumbnails.forget_library(library);
    }
}
