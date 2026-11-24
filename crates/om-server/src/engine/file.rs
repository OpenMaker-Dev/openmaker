//! The file engine: each hosted file one `.fmp12`, loaded whole into a
//! `Store` and saved whole (docs/server-storage.md 1, 5).

use log::{info, warn};

use super::transaction::{Hold, Locks, Transaction};
use super::{run, Environment, HostEnvironment, Operation, Outcome};
use crate::hosting::Hosted;

/// The file engine. Its environment is the one operations read the clock
/// and randomness from ([`Environment`]).
pub struct FileEngine {
    environment: Box<dyn Environment>,
}

impl Default for FileEngine {
    fn default() -> Self {
        Self::new(Box::new(HostEnvironment))
    }
}

/// What of the file the engine holds for an operation: what today's code
/// took for each request. op61, op62 and op65 never took the store, and
/// op60 only read it.
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
    /// op72: the file saved if it changed, and a failed save not reported,
    /// as `Hosted::save` does.
    Unchecked,
}

fn needs(operation: &Operation) -> Needs {
    match operation {
        Operation::ReserveRecordIds(_)
        | Operation::AdvanceSerial(_)
        | Operation::CommitRecords(_) => Needs::Transaction,
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
        Operation::AdvanceSerial(_) => Save::Unchecked,
        _ => Save::Nothing,
    }
}

impl FileEngine {
    pub fn new(environment: Box<dyn Environment>) -> Self {
        FileEngine { environment }
    }

    /// Run `operation` on `file` and store what it changed before
    /// returning its outcome, which the caller then sends: the reply
    /// encoded, the notices delivered. Operations on one file run one at a
    /// time under the store's write lock, or the lock table's mutex for
    /// those that only edit it (lock order: the module doc of
    /// [`super`]). When what it changed cannot be saved, it is taken back,
    /// locks it took included, and the outcome is the storage error with
    /// no notices.
    pub fn submit(&self, file: &Hosted, operation: Operation) -> Outcome {
        let environment = &*self.environment;
        match needs(&operation) {
            Needs::LockTable => {
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
            Needs::Transaction => self.submit_write(file, operation),
        }
    }

    fn submit_write(&self, file: &Hosted, operation: Operation) -> Outcome {
        let save = save(&operation);
        let mut store = file.write_store();
        let mut locks = Locks::new(&file.locks);
        let mut tx = Transaction::new(&mut store);
        let outcome = run(
            operation,
            &file.name,
            Hold::Transaction(&mut tx),
            &mut locks,
            &*self.environment,
        );
        let Transaction {
            journal,
            reservation,
            stored_lines,
            ..
        } = tx;
        let saved = match (journal, save) {
            (Some(journal), _) => file.save_commit(&mut store, journal),
            (None, Save::Reservation) if reservation.is_some() => file.save_held(&mut store),
            (None, Save::Unchecked) => {
                // A failed save leaves the change in memory for the next
                // one, and the reply does not say so (#673 step 1's
                // follow-up reports it).
                if !store.is_read_only() {
                    let _ = file.save_held(&mut store);
                }
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
