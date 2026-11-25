//! The schema operations of [`super::run`]: op13's catalog uploads, what
//! an op86 program with schema instructions stores, a find's indexes,
//! op73's table updates and the deferred field work a file is opened with
//! (schema-path.md 3.3, 3.4, 3.6, 7.2; query-program.md 7.1). Each change
//! goes into the transaction's journal, which the engine stores or takes
//! back (`FileEngine::submit`).

use std::borrow::Cow;
use std::collections::BTreeMap;

use log::{error, warn};
use om_hbam::{OwnedNode, RidList};
use om_query::Write;

use super::{
    ChangeSchema, Environment, ReadError, Transaction, UpdateTable, UploadToCatalog, View, NO_LIMIT,
};
use crate::derived::{self, counts_key, CATALOG_INDEX, LAST_MEMBER_ID};
use crate::library::LIBRARY;
use crate::modcount::{self, MOD_COUNT};
use crate::store::keys::INDEXES;
use crate::store::{IndexId, MemberKey};

/// The refusal of a write to a file hosted read-only.
const READ_ONLY: &str = "the file is hosted read-only";

/// op13: put the subtree at its path (schema-path.md 3.3), creating missing
/// parents; one the file holds already is stored as no change (the
/// transaction's). Refused, with the tree unchanged: on a file hosted
/// read-only, decided here under the write lock before anything else, also
/// when the file holds the subtree already (#737); over the whole file; and
/// where the file cannot hold it (hbam-writer.md 2.2, 2.3,
/// `om_hbam::writer::put_refusals`), logged. The request's reply must not
/// say done for any of them: op13 answers each as an upload whose save
/// failed.
pub(super) fn upload_to_catalog(
    op: UploadToCatalog,
    file: &str,
    tx: &mut Transaction<'_>,
) -> Result<(), String> {
    if tx.is_read_only() {
        return Err(READ_ONLY.to_owned());
    }
    if op.path.is_empty() {
        warn!("{file}: refusing an upload over the whole file");
        return Err("an upload over the whole file".to_owned());
    }
    let path = op.path.clone();
    tx.replace(op.path, op.node).map_err(|reason| {
        warn!("{file}: refusing an upload to {path:02x?} the file cannot hold: {reason}");
        format!("the file cannot hold it there: {reason}")
    })
}

/// op86's writes, after the program ran: refused, with nothing stored, when
/// the file became read-only since and the program wrote. Otherwise the
/// indexes are stored, also on a read-only file, where they stay in memory;
/// then the member keys are written and the writes made.
pub(super) fn change_schema(
    op: ChangeSchema,
    file: &str,
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
    store_indexes(indexes, file, tx);
    if !tx.is_read_only() {
        write_member_keys(member_keys, file, tx);
        apply(writes, file, tx);
    }
    Ok(())
}

/// The indexes finds built on demand (an op86 program's, a find's
/// `StoreIndexes`; `Store::take_indexes_on_demand`), stored so that record
/// writes keep them current from now on (query-program.md 7.1): each built
/// again from the records as the transaction reads them
/// (`derived::index_entries`) and written whole with its counts. An index
/// stored already (its counts there), a field whose index is not built on
/// demand, and one in a language whose keys this server does not build
/// while a record holds text there (#572; one built while none did, a
/// record of which has gained text since, is not stored, #159) are left as
/// they are; one the file cannot hold is logged and left.
pub(super) fn store_indexes(indexes: Vec<IndexId>, file: &str, tx: &mut Transaction<'_>) {
    #[cfg(test)]
    if super::transaction::PANIC_STORING_INDEXES.with(std::cell::Cell::get) {
        panic!("a test's panic while storing indexes");
    }
    if indexes.is_empty() {
        return;
    }
    let derived = tx.derived();
    for (table, field, kind) in indexes {
        let counts = counts_key(kind);
        let view: &dyn View = tx;
        let built = (|| -> Result<Option<BTreeMap<Vec<u8>, RidList>>, ReadError> {
            let at: [&[u8]; 4] = [&table, &[INDEXES], &field, &[counts]];
            if view.node(&at)?.is_some() {
                return Ok(None);
            }
            let Some(data_type) = derived.indexable(&table, &field, kind) else {
                return Ok(None);
            };
            let Some(form) = derived::written_form(view, &derived, &table, &field, data_type)?
            else {
                return Ok(None);
            };
            let keys = derived::index_entries(
                view, &derived, &table, &field, data_type, kind, form, false,
            )?;
            Ok(Some(keys.unwrap_or_default()))
        })();
        let keys = match built {
            Ok(Some(keys)) => keys,
            Ok(None) => continue,
            Err(error) => {
                warn!(
                    "{file}: index {kind} of table {table:02x?} field {field:02x?} not stored: \
                     {error}"
                );
                continue;
            }
        };
        let entries: usize = keys.values().map(RidList::len).sum();
        let distinct = keys.len();
        let path = |key: u8| vec![table.clone(), vec![INDEXES], field.clone(), vec![key]];
        let mut written = Ok(());
        if !keys.is_empty() {
            let nodes = keys
                .into_iter()
                .map(|(key, ids)| ids.to_node(key))
                .collect();
            written = tx.replace(path(kind), OwnedNode::list(vec![kind], nodes));
        }
        if written.is_ok() {
            let mut value = (entries as u64).to_be_bytes().to_vec();
            value.extend((distinct as u64).to_be_bytes());
            written = tx.put(path(counts), OwnedNode::leaf(Vec::new(), value));
        }
        match written {
            Ok(()) => tx.once_stored(format!(
                "built index {kind} of table {table:02x?} field {field:02x?} on demand"
            )),
            Err(reason) => error!(
                "{file}: index {kind} of table {table:02x?} field {field:02x?} cannot be \
                 stored: {reason}"
            ),
        }
    }
}

/// GenerateMemberKey's issued keys: each catalog's last member id and
/// count (`[C, 01, 00]`, `[C, 01, fc]`, schema-path.md 3.4), where they
/// advance; a key the index has passed already stays. A catalog whose
/// index cannot take them (checked again here: the tree may have changed
/// since the keys were issued) is logged and left as it is.
fn write_member_keys(keys: Vec<MemberKey>, file: &str, tx: &mut Transaction<'_>) {
    if !keys.is_empty() {
        tx.wrote_member_keys();
    }
    for (catalog, (id, count)) in keys {
        let path: Vec<&[u8]> = catalog.iter().map(Vec::as_slice).collect();
        let refused = crate::derived::member_key_refusal(&*tx, &path)
            .unwrap_or_else(|error| Some(error.to_string()));
        if let Some(reason) = refused {
            warn!("{file}: member keys issued for {path:02x?} are not written: {reason}");
            continue;
        }
        for (key, value) in [(LAST_MEMBER_ID, id), (MOD_COUNT, count)] {
            let mut at = catalog.clone();
            at.extend([vec![CATALOG_INDEX], vec![key]]);
            let segments: Vec<&[u8]> = at.iter().map(Vec::as_slice).collect();
            let current = tx
                .subtree(&segments, NO_LIMIT)
                .ok()
                .flatten()
                .map(Cow::into_owned);
            // Changed only when a counter advances: keys a write already
            // passed change nothing to save.
            if modcount::serial_value(current.as_ref()) < value {
                if let Err(reason) = tx.put(at, modcount::serial_leaf(key, value)) {
                    warn!("{file}: member keys issued for {path:02x?} are not written: {reason}");
                }
            }
        }
    }
}

/// The changes of an op86 program's schema instructions (UploadMultiple,
/// DeleteTable; schema-path.md 3.4). A put the file cannot hold is logged
/// and left out (om-query refuses these before they get here). A deleted
/// table takes its container data with it (`[T, 1f]`, containers.md 1.1),
/// every member at once, so no reference is left to release; the engine
/// forgets what it derived from them (thumbnails) once the change is
/// stored.
fn apply(writes: Vec<Write>, file: &str, tx: &mut Transaction<'_>) {
    for write in writes {
        match write {
            Write::Put { path, node } => {
                if path.is_empty() {
                    warn!("{file}: refusing a write over the whole file");
                    continue;
                }
                let at = path.clone();
                if let Err(reason) = tx.replace(path, node) {
                    warn!("{file}: refusing a write to {at:02x?} the file cannot hold: {reason}");
                }
            }
            Write::Remove { path } => {
                if let [table] = path.as_slice() {
                    let members = [
                        table.as_slice(),
                        &[LIBRARY],
                        &[om_data::containers::keys::MEMBERS],
                    ];
                    match View::count_children(&*tx, &members) {
                        Ok(0) => {}
                        Ok(count) => tx.once_stored(format!(
                            "{file}: table {table:02x?} deleted with its {count} container \
                             library member(s)"
                        )),
                        Err(error) => warn!(
                            "{file}: the container library of table {table:02x?}, deleted, \
                             could not be counted: {error}"
                        ),
                    }
                    tx.release_library(vec![table.clone(), vec![LIBRARY]]);
                }
                if !path.is_empty() {
                    let _ = tx.remove(path);
                }
            }
        }
    }
}

/// op73: whether the table's field work changed the file. A recalculation
/// takes each record's environment (the time, a UUID) and the unique ids of
/// container data it stores from `environment`; the formulas it evaluates
/// do not yet ([`Environment`]).
pub(super) fn update_table(
    op: UpdateTable,
    tx: &mut Transaction<'_>,
    environment: &dyn Environment,
) -> Result<bool, crate::store::NotUpdated> {
    let UpdateTable { table, account } = op;
    // Calculations that read `Get ( AccountName )` see the account that
    // saved the change.
    let (mut record_environment, mut unique_id) = recalculation(&account, environment);
    tx.update_table(&table, &mut record_environment, &mut unique_id)
}

/// The deferred field work the file records as pending, finished as op73
/// would have done it (`Store::recover`): whether it changed the file, or
/// the work that cannot be done. Its calculations see no account, as no
/// one saved the change in this session; the rest is [`update_table`]'s.
pub(super) fn finish_deferred_work(
    tx: &mut Transaction<'_>,
    environment: &dyn Environment,
) -> Result<bool, crate::store::WorkKept> {
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

#[cfg(test)]
mod tests {
    use om_hbam::OwnedNode;
    use om_query::{Source, Write};

    use super::{apply, write_member_keys};
    use crate::engine::transaction::taken_back;
    use crate::engine::written;
    use crate::library::LIBRARY;
    use crate::store::tests::store_of;

    #[test]
    fn deleted_tables_take_their_libraries() {
        // A table with a container library member and a basis of stored
        // calculations, and another table with a basis.
        let mut root = OwnedNode::default();
        let member = [&[0x80, 0x02][..], &[LIBRARY], &[5], &[1], &[0x12]];
        root.put(&member, OwnedNode::leaf(Vec::new(), vec![0, 0, 0, 1]));
        root.put(
            &[&[0x80, 0x03], &[0]],
            OwnedNode::leaf(Vec::new(), vec![1, 1]),
        );
        let mut store = store_of(root);
        for table in [[0x80, 0x02], [0x80, 0x03]] {
            store.set_basis_in(None, &table, Some(Default::default()));
        }
        // DeleteTable (schema-path.md 3.4): the library goes with [T], and
        // its basis with it (review of #811, round 1), so that a table made
        // later at the same key starts without one. Taken back, as a save
        // that failed takes it back, the table and its basis are as they
        // were.
        let remove = |path: Vec<Vec<u8>>| vec![Write::Remove { path }];
        taken_back(&mut store, |tx| {
            apply(remove(vec![vec![0x80, 0x02]]), "", tx)
        });
        assert!(store.node(&member).is_some());
        let bases: Vec<&Vec<u8>> = store.bases().keys().collect();
        assert_eq!(bases, [&vec![0x80, 0x02], &vec![0x80, 0x03]]);
        let released = written(&mut store, |tx| {
            apply(remove(vec![vec![0x80, 0x02]]), "", tx);
            std::mem::take(&mut tx.released_libraries)
        });
        assert_eq!(released, vec![vec![vec![0x80, 0x02], vec![LIBRARY]]]);
        assert!(store.node(&member).is_none());
        assert!(store.node(&[&[0x80, 0x03]]).is_some());
        let bases: Vec<&Vec<u8>> = store.bases().keys().collect();
        assert_eq!(bases, [&vec![0x80, 0x03]]);
        // Removing a catalog member is no table's deletion.
        let released = written(&mut store, |tx| {
            apply(
                remove(vec![vec![3], vec![0x10], vec![5], vec![0x80, 0x02]]),
                "",
                tx,
            );
            std::mem::take(&mut tx.released_libraries)
        });
        assert!(released.is_empty());
    }

    /// The member ids and counts GenerateMemberKey issued go into their
    /// catalogs' indexes (live:s4 g_41_after, s5), and the next key is
    /// issued from there.
    #[test]
    fn issued_member_keys_go_into_the_indexes() {
        // live:s4: [4, 1, 00] = 01 04 and fc = 01 0d give 05/0e, then 06/0f;
        // live:s4b: [3, 16, 1, 00] = 02 80 05 and fc = 01 02 give 80 06/03.
        let mut root = OwnedNode::default();
        root.put(&[&[4], &[1], &[0]], OwnedNode::leaf(Vec::new(), vec![1, 4]));
        root.put(
            &[&[4], &[1], &[0xfc]],
            OwnedNode::leaf(Vec::new(), vec![1, 0x0d]),
        );
        let tables: [&[u8]; 3] = [&[3], &[0x10], &[1]];
        root.put(
            &[tables[0], tables[1], tables[2], &[0]],
            OwnedNode::leaf(Vec::new(), vec![2, 0x80, 0x05]),
        );
        root.put(
            &[tables[0], tables[1], tables[2], &[0xfc]],
            OwnedNode::leaf(Vec::new(), vec![1, 2]),
        );
        let mut store = store_of(root);
        let layouts = vec![vec![4]];
        for catalog in [&layouts[..], &layouts, &[vec![3], vec![0x10]], &layouts] {
            store.generate_member_key(catalog).unwrap();
        }
        let issued = store.take_issued_member_keys();
        written(&mut store, |tx| write_member_keys(issued, "", tx));
        let value = |path: &[&[u8]]| store.node(path).unwrap().value.clone().unwrap();
        assert_eq!(value(&[&[4], &[1], &[0]]), [1, 7]);
        assert_eq!(value(&[&[4], &[1], &[0xfc]]), [1, 0x10]);
        assert_eq!(value(&[&[3], &[0x10], &[1], &[0]]), [2, 0x80, 0x06]);
        assert_eq!(
            store.generate_member_key(&layouts).unwrap(),
            (vec![1, 8], vec![1, 0x11])
        );
    }
}
