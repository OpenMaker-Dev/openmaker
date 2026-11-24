//! The file engine: each hosted file one `.fmp12`, loaded whole into a
//! `Store` and saved whole (docs/server-storage.md 1, 5).

use log::{info, warn};

use super::transaction::{self, Hold, Locks, Transaction};
use super::{
    run, ChangeSchema, Credentials, Draft, Edited, Environment, FileNotSaved, HostEnvironment,
    Operation, Outcome, Reply, StoreIndexes,
};
use crate::hosting::{Hosted, StoreWriteGuard};
use crate::store::{MemberKey, Store};

/// The file engine. Its environment is the one operations read the clock
/// and randomness from ([`Environment`]).
pub struct FileEngine {
    environment: Box<dyn Environment>,
    recorder: Option<Recorder>,
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
/// took for each request. op61, op62 and op65 never took the store, and
/// op60 only read it. op13, op73, op86's writes, a find's indexes, op87,
/// op88, op240's save, the account edits, the deferred field work finished
/// at opening and a saved found set took the store for writing and never
/// the lock table, which their runs do not use (a found set only when no
/// one held the store, [`FileEngine::try_submit`]); an authoring edit took
/// the store for writing and the lock table only while it took and
/// released its locks.
enum Needs {
    LockTable,
    View,
    Transaction,
}

/// How the engine saves what an operation changed: as each request saved
/// before the engine interface, which #673 step 1's follow-up makes one
/// (every change saved before it is answered, a failed save reported).
enum Save {
    /// Nothing to save: lock-table edits only, a server script's op71,
    /// whose reservation its commit saves, or the deferred field work
    /// finished at opening, whose save waits (`Hosted::commit_recovery`).
    Nothing,
    /// The operation's commit (`Hosted::save_commit`); a failed save takes
    /// it back.
    Commit,
    /// A window's saved found set: its commit saved by the store alone
    /// (`Store::save_commit`), as the desktop always saved it, so a failed
    /// save, which takes it back, raises no alert and logs only the
    /// engine's warning.
    QuietCommit,
    /// A client's op71: its reservation is saved before the ids are
    /// answered (`Hosted::save_held`); a failed save takes it back.
    Reservation,
    /// op72, op13, op86's writes, a find's indexes, op87 and op88: the file
    /// saved if it changed, and a failed save not reported, as
    /// `Hosted::save` does and as op13 dropped the result of
    /// `Hosted::save_held`.
    Unchecked,
    /// op73: [`Save::Unchecked`] when its work changed the file, which is
    /// otherwise not saved.
    UncheckedIfChanged,
    /// op240's save: [`Save::Unchecked`] when the file may be written, the
    /// reply then saying whether it holds changes the disk lacks
    /// (`Store::has_unsaved_changes`), as op240 always answered it.
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
        | Operation::ChangePassword(_)
        | Operation::ResetPassword(_)
        | Operation::EnableAccount(_)
        | Operation::DeleteAccount(_)
        | Operation::FinishDeferredWork
        | Operation::SaveFoundSet(_) => Needs::Transaction,
        Operation::TakeLock(_) => Needs::View,
        Operation::UpgradeLock(_) | Operation::ReleaseLock(_) | Operation::ReleaseLocks(_) => {
            Needs::LockTable
        }
    }
}

fn save(operation: &Operation) -> Save {
    match operation {
        Operation::ReserveRecordIds(op) if op.durable => Save::Reservation,
        // An authoring edit or an account edit that changed the file did
        // it in a commit.
        Operation::CommitRecords(_)
        | Operation::EditDesign(_)
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
        | Operation::AdjustContainerCount(_) => Save::Unchecked,
        Operation::UpdateTable(_) => Save::UncheckedIfChanged,
        Operation::SaveFile => Save::Requested,
        _ => Save::Nothing,
    }
}

/// Complete `draft` with what reads left waiting in `store`, which the
/// engine holds for writing (the module doc of [`super`], "Drafts"): all of
/// it, as the first write to hold the store took it before the engine
/// interface, and only what the run will store. Writes refused on a
/// read-only file take nothing, and member keys stay waiting on a
/// read-only file. Also returns the member keys taken, which go back to
/// waiting if the run is left by a panic before it writes them, as they
/// stayed waiting before when storing the indexes panicked.
fn complete(draft: Draft, store: &mut Store) -> (Operation, Vec<MemberKey>) {
    match draft {
        Draft::ChangeSchema { writes } => {
            if store.is_read_only() && !writes.is_empty() {
                let refused = Operation::ChangeSchema(ChangeSchema {
                    writes,
                    member_keys: Vec::new(),
                    indexes: Vec::new(),
                });
                return (refused, Vec::new());
            }
            let indexes = store.take_indexes_on_demand();
            let member_keys = if store.is_read_only() {
                Vec::new()
            } else {
                store.take_issued_member_keys()
            };
            let taken = member_keys.clone();
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
            (
                Operation::StoreIndexes(StoreIndexes { indexes }),
                Vec::new(),
            )
        }
    }
}

/// Save the file if it changed, as `Hosted::save` does: not a read-only
/// one, and a failed save is not reported.
fn save_unchecked(file: &Hosted, store: &mut Store) {
    // A failed save leaves the change in memory for the next one, and the
    // reply does not say so (#673 step 1's follow-up reports it).
    if !store.is_read_only() {
        let _ = file.save_held(store);
    }
}

impl FileEngine {
    pub fn new(environment: Box<dyn Environment>) -> Self {
        FileEngine {
            environment,
            recorder: None,
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
    /// [`super`]). When what op70, a client's op71, an authoring edit, an
    /// account edit or a saved found set changed cannot be saved, it is
    /// taken back, locks it took included, and the outcome is the storage
    /// error with no notices; the other
    /// operations' failed saves are not reported yet ([`Save`]), op240's
    /// save excepted, whose reply is whether the file was saved.
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
                let mut locks = Locks::new(&file.locks);
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
                let mut locks = Locks::new(&file.locks);
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
            Needs::Transaction => self.submit_write(file, file.write_store(), credentials, |_| {
                (operation, Vec::new())
            }),
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
                let store = file.try_write_store()?;
                Some(self.submit_write(file, store, Credentials::None, |_| (operation, Vec::new())))
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
        self.submit_write(file, file.write_store(), Credentials::None, |store| {
            complete(draft, store)
        })
    }

    /// Run the operation `operation` gives for `store`, `file`'s, which
    /// the caller took for writing, with the member keys it took from the
    /// waiting ones ([`complete`]): if the run is left by a panic before it
    /// writes them, they wait again.
    fn submit_write(
        &self,
        file: &Hosted,
        mut store: StoreWriteGuard<'_>,
        credentials: Credentials,
        operation: impl FnOnce(&mut Store) -> (Operation, Vec<MemberKey>),
    ) -> Outcome {
        let (operation, taken) = operation(&mut store);
        self.record(&operation);
        let save = save(&operation);
        let mut locks = Locks::new(&file.locks);
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
                let wrote = tx.wrote_member_keys;
                drop(tx);
                if !wrote {
                    store.put_back_member_keys(taken);
                }
                std::panic::resume_unwind(panic)
            }
        };
        // An authoring edit's locks, released once the store is let go.
        let held = locks.after_stored();
        let Transaction {
            journal,
            reservation,
            stored_lines,
            released_libraries,
            field_bases,
            ..
        } = tx;
        if !released_libraries.is_empty() {
            // A deleted table's library is gone: so are its thumbnails.
            let mut thumbnails = file.thumbnails.lock().expect("thumbnail lock");
            for library in &released_libraries {
                thumbnails.forget_library(library);
            }
        }
        let changed = matches!(outcome.reply, Reply::UpdateTable(Ok(true)));
        let saved = match (journal, save) {
            (Some(journal), Save::QuietCommit) => store.save_commit(&file.at, journal),
            (Some(journal), _) => file.save_commit(&mut store, journal),
            (None, Save::Reservation) if reservation.is_some() => file.save_held(&mut store),
            (None, Save::Unchecked) => {
                save_unchecked(file, &mut store);
                Ok(())
            }
            (None, Save::UncheckedIfChanged) if changed => {
                save_unchecked(file, &mut store);
                Ok(())
            }
            (None, Save::Requested) if outcome.reply == Reply::SaveFile(Ok(())) => {
                save_unchecked(file, &mut store);
                if store.has_unsaved_changes() {
                    outcome.reply = Reply::SaveFile(Err(FileNotSaved::Failed));
                }
                Ok(())
            }
            _ => Ok(()),
        };
        let outcome = match saved {
            Ok(()) => {
                locks.keep();
                for (table, field) in field_bases {
                    store.add_field_basis(&table, field);
                }
                for line in stored_lines {
                    info!("{line}");
                }
                outcome
            }
            Err(error) => {
                locks.take_back();
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
                if let Some(reservation) = reservation {
                    store.take_back_record_ids(
                        &reservation.table,
                        reservation.before,
                        reservation.dirty,
                    );
                    warn!(
                        "{:?}: no record ids reserved in table {:02x?}: the file could not be \
                         saved: {error}",
                        file.name, reservation.table
                    );
                }
                outcome.not_stored(&error)
            }
        };
        drop(store);
        transaction::release(&file.locks, held);
        outcome
    }
}
