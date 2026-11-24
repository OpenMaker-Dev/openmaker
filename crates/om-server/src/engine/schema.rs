//! The schema operations of [`super::run`]: op13's catalog uploads, what
//! an op86 program with schema instructions stores, a find's indexes,
//! op73's table updates and the deferred field work a file is opened with
//! (schema-path.md 3.3, 3.4, 3.6, 7.2; query-program.md 7.1). They change the file's tree in place: their
//! saves are not checked yet, so nothing of them is taken back
//! (`FileEngine::submit`).

use super::{ChangeSchema, Environment, Transaction, UpdateTable, UploadToCatalog};

/// The refusal of a write to a file hosted read-only.
const READ_ONLY: &str = "the file is hosted read-only";

/// op13: whether the subtree was stored. Nothing is on a read-only file.
pub(super) fn upload_to_catalog(op: UploadToCatalog, tx: &mut Transaction<'_>) -> bool {
    !tx.is_read_only() && tx.put_subtree(&op.path, op.node)
}

/// op86's writes, after the program ran: refused, with nothing stored, when
/// the file became read-only since and the program wrote. Otherwise the
/// indexes are stored, also on a read-only file, where they stay in memory;
/// then the member keys are written and the writes made.
pub(super) fn change_schema(
    op: ChangeSchema,
    tx: &mut Transaction<'_>,
) -> Result<(), &'static str> {
    let ChangeSchema {
        writes,
        member_keys,
        indexes,
    } = op;
    if tx.is_read_only() && !writes.is_empty() {
        return Err(READ_ONLY);
    }
    tx.store_indexes(indexes);
    if !tx.is_read_only() {
        tx.write_member_keys(member_keys);
        tx.apply(writes);
    }
    Ok(())
}

/// op73: whether the table's field work changed the file. A recalculation
/// takes each record's environment (the time, a UUID) and the unique ids of
/// container data it stores from `environment`; the formulas it evaluates
/// do not yet ([`Environment`]).
pub(super) fn update_table(
    op: UpdateTable,
    tx: &mut Transaction<'_>,
    environment: &dyn Environment,
) -> Result<bool, &'static str> {
    let UpdateTable { table, account } = op;
    // Calculations that read `Get ( AccountName )` see the account that
    // saved the change.
    let (mut record_environment, mut unique_id) = recalculation(&account, environment);
    tx.update_table(&table, &mut record_environment, &mut unique_id)
}

/// The deferred field work the file records as pending, finished as op73
/// would have done it (`Store::recover`): whether it changed the file.
/// Its calculations see no account, as no one saved the change in this
/// session; the rest is [`update_table`]'s.
pub(super) fn finish_deferred_work(
    tx: &mut Transaction<'_>,
    environment: &dyn Environment,
) -> bool {
    let (mut record_environment, mut unique_id) = recalculation("", environment);
    tx.recover(&mut record_environment, &mut unique_id)
}

/// What a recalculation of stored values takes from `environment`: each
/// record's environment, whose calculations see `account`
/// ([`crate::recalc::record_environment_in`]), and the unique ids of the
/// container data it stores. The one place an operation's recalculation
/// builds them (op73 and the deferred work finished at opening).
fn recalculation<'a>(
    account: &'a str,
    environment: &'a dyn Environment,
) -> (
    impl FnMut() -> om_data::rules::RecordEnvironment + 'a,
    impl FnMut() -> [u8; 16] + 'a,
) {
    let record_environment = move || crate::recalc::record_environment_in(account, environment);
    let unique_id = move || {
        let mut id = [0u8; 16];
        environment.fill_random(&mut id);
        id
    };
    (record_environment, unique_id)
}
