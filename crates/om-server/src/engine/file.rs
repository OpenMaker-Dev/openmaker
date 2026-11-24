//! The file engine: each hosted file one `.fmp12`, loaded whole into a
//! `Store` and saved whole (docs/server-storage.md 1, 5).

use log::{info, warn};

use super::transaction::{Hold, Locks, Transaction};
use super::{
    run, ChangeSchema, Draft, Environment, HostEnvironment, Operation, Outcome, Reply, StoreIndexes,
};
use crate::hosting::Hosted;
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
type Recorder = Box<dyn Fn(&Operation) + Send + Sync>;

impl Default for FileEngine {
    fn default() -> Self {
        Self::new(Box::new(HostEnvironment))
    }
}

/// What of the file the engine holds for an operation: what today's code
/// took for each request. op61, op62 and op65 never took the store, and
/// op60 only read it. op13, op73, op86's writes and a find's indexes took
/// the store for writing and never the lock table, which their runs do not
/// use.
enum Needs {
    LockTable,
    View,
    Transaction,
}

/// How the engine saves what an operation changed: as each request saved
/// before the engine interface, which #673 step 1's follow-up makes one
/// (every change saved before it is answered, a failed save reported).
enum Save {
    /// Nothing to save: lock-table edits only, or a server script's op71,
    /// whose reservation its commit saves.
    Nothing,
    /// The operation's commit (`Hosted::save_commit`); a failed save takes
    /// it back.
    Commit,
    /// A client's op71: its reservation is saved before the ids are
    /// answered (`Hosted::save_held`); a failed save takes it back.
    Reservation,
    /// op72, op13, op86's writes and a find's indexes: the file saved if it
    /// changed, and a failed save not reported, as `Hosted::save` does and
    /// as op13 dropped the result of `Hosted::save_held`.
    Unchecked,
    /// op73: [`Save::Unchecked`] when its work changed the file, which is
    /// otherwise not saved.
    UncheckedIfChanged,
}

fn needs(operation: &Operation) -> Needs {
    match operation {
        Operation::ReserveRecordIds(_)
        | Operation::AdvanceSerial(_)
        | Operation::CommitRecords(_)
        | Operation::UploadToCatalog(_)
        | Operation::ChangeSchema(_)
        | Operation::StoreIndexes(_)
        | Operation::UpdateTable(_) => Needs::Transaction,
        Operation::TakeLock(_) => Needs::View,
        Operation::UpgradeLock(_) | Operation::ReleaseLock(_) | Operation::ReleaseLocks(_) => {
            Needs::LockTable
        }
    }
}

fn save(operation: &Operation) -> Save {
    match operation {
        Operation::ReserveRecordIds(op) if op.durable => Save::Reservation,
        Operation::CommitRecords(_) => Save::Commit,
        Operation::AdvanceSerial(_)
        | Operation::UploadToCatalog(_)
        | Operation::ChangeSchema(_)
        | Operation::StoreIndexes(_) => Save::Unchecked,
        Operation::UpdateTable(_) => Save::UncheckedIfChanged,
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
    /// [`super`]). When what op70 or a client's op71 changed cannot be
    /// saved, it is taken back, locks it took included, and the outcome is
    /// the storage error with no notices; the other operations' failed
    /// saves are not reported yet ([`Save`]).
    pub fn submit(&self, file: &Hosted, operation: Operation) -> Outcome {
        let environment = &*self.environment;
        match needs(&operation) {
            Needs::LockTable => {
                self.record(&operation);
                let mut locks = Locks::new(&file.locks);
                let outcome = run(
                    operation,
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
                    &file.name,
                    Hold::View(&store),
                    &mut locks,
                    environment,
                );
                locks.keep();
                outcome
            }
            Needs::Transaction => self.submit_write(file, |_| (operation, Vec::new())),
        }
    }

    /// Complete `draft` with what reads left waiting in `file`, under the
    /// store's write lock, and run the operation it becomes as
    /// [`Self::submit`] does, in the same hold. A recording gets the
    /// completed operation ([`Self::recording`]), which [`Self::submit`]
    /// replays.
    pub fn submit_draft(&self, file: &Hosted, draft: Draft) -> Outcome {
        self.submit_write(file, |store| complete(draft, store))
    }

    /// Run the operation `operation` gives for the store, which it is
    /// given under the write lock, with the member keys it took from the
    /// waiting ones ([`complete`]): if the run is left by a panic before it
    /// writes them, they wait again.
    fn submit_write(
        &self,
        file: &Hosted,
        operation: impl FnOnce(&mut Store) -> (Operation, Vec<MemberKey>),
    ) -> Outcome {
        let mut store = file.write_store();
        let (operation, taken) = operation(&mut store);
        self.record(&operation);
        let save = save(&operation);
        let mut locks = Locks::new(&file.locks);
        let mut tx = Transaction::new(&mut store);
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(
                operation,
                &file.name,
                Hold::Transaction(&mut tx),
                &mut locks,
                &*self.environment,
            )
        }));
        let outcome = match ran {
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
        let Transaction {
            journal,
            reservation,
            stored_lines,
            released_libraries,
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
            _ => Ok(()),
        };
        match saved {
            Ok(()) => {
                locks.keep();
                for line in stored_lines {
                    info!("{line}");
                }
                outcome
            }
            Err(error) => {
                locks.take_back();
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
        }
    }
}
