//! Script transactions on the hosted file (script-runtime.md 8), on the
//! record write path (write-path.md, record-writes.md).
//!
//! Open Transaction takes one write transaction id for everything until
//! Commit or Revert Transaction. Inside it:
//!
//! - New Record reserves the record id at once with op71 under that id, as
//!   FileMaker Pro does (write-path.md 1.1); the reservation locks it.
//! - The first change or deletion of an existing record locks it with op60
//!   under the transaction (refused: 301; changed by another client since
//!   it was read: 306), and the lock is held until the transaction ends.
//! - The values set are held here, not uploaded; record pages show them,
//!   new records join the table and deleted ones leave it. Finds run on the
//!   host and see its committed records only; calculations other than the
//!   window's current record read committed values too.
//!
//! Commit Transaction applies the record rules (`om_data::rules`) to every
//! held record with the transaction's options: auto-enter (unless "Skip
//! auto-enter options"), stored calculations and validation (fields
//! validated only during data entry skipped with "Skip data entry
//! validation"), unique values against the host's indexes (ExistsInIndex
//! under the transaction, write-path.md 1.3) and among the held records.
//! No record data is submitted before every record passes (the locks and
//! op71 reservations its changes took are held already); then each table's records
//! are uploaded (op14) and one op70 lists every record of every table
//! under the transaction, so the host applies them together. Every record
//! is read back, then op65 releases the transaction's locks. A refusal
//! reverts the whole transaction and is Commit Transaction's error.
//! Revert Transaction sends only op65; reserved ids stay used (FileMaker
//! does not give them back either).
//!
//! Commit runs in two stages that can be called apart (#1006):
//! [`NativeProvider::prepare_commit`] applies the rules and builds every
//! upload, and [`NativeProvider::submit_commit`] sends op14 and op70 and
//! reads the records back. Preparing sends no op14 or op70, but it is not
//! free of effects on the host: it reserves the serial numbers not taken
//! yet (op72; the values stay used whatever follows, write-path.md 3.2) and
//! reads indexes, related records and container data. A prepared commit
//! keeps the transaction's locks and container references until it is
//! submitted or abandoned ([`NativeProvider::abandon_commit`]). `commit`
//! is the two stages in a row.
//!
//! Each transaction has a [`TransactionKind`], and a related file's
//! provider on the same connection joins it under its id
//! ([`NativeProvider::begin_under`]), as FileMaker Pro commits two files'
//! records under one transaction (#1006; N1's synthetic run). The joined
//! file's locks are the transaction's, and ending its part sends no op65:
//! that would release every participant's locks. The opened file's
//! coordinator (related_owner.rs) ends the transaction once, after every
//! participant has settled. Nothing commits records of several files in one
//! commit yet: related_owner.rs keeps each commit to one file.

use super::records::{
    apply_values, entered, expected_values, mod_count_value, stored_record, text_values,
    CurrentSet, Origin, MOD_COUNT, RECORD_MISSING, STALE_RECORD,
};
use super::*;
use om_data::found::code;
use om_data::rules::{
    error, Entry, HeldRecords, RecordEnvironment, WriteContext, WriteMode, Written,
};
use om_data::runtime::TransactionOptions;
use om_data::FieldChanges;
use om_hbam::OwnedNode;
use om_protocol::write;

/// Whose transaction it is (#1006): a window's commit of a record with its
/// related records, or a script's Open Transaction. Both commit alike
/// today; the kind is kept so that they can differ once FileMaker's
/// handling of a later file's refusal is measured for each. A related
/// file's transaction takes the kind of the transaction it joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransactionKind {
    /// The window's commit of a record with its related records
    /// (`DataProvider::begin_record_commit`).
    Record,
    Script,
}

/// An open transaction as another file's provider joins it
/// ([`NativeProvider::begin_under`]). Made only from the open transaction
/// ([`NativeProvider::shared_transaction`]): the joining file takes its
/// connection's identity, id, kind and options, never its own.
#[derive(Clone, Debug)]
pub(crate) struct SharedTransaction {
    connection: om_session::ConnectionRef,
    txn: u32,
    kind: TransactionKind,
    options: TransactionOptions,
}

/// Issued scalar creation state, opaque outside the transaction engine.
#[derive(Clone, Default)]
pub(crate) struct ScalarRecords(Vec<CarriedRecord>);

impl ScalarRecords {
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
    pub(crate) fn empty() -> Self {
        Self(Vec::new())
    }
    pub(crate) fn single_table(&self) -> Option<u64> {
        let [((table, _), _, _)] = self.0.as_slice() else {
            return None;
        };
        Some(*table)
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn contains(&self, table: u64, record: u64) -> bool {
        self.0.iter().any(|(key, _, _)| *key == (table, record))
    }
    // Restaging a caller's already accepted container cell requires no new
    // acquisition authority. It must remain exactly the retained held value.
    pub(crate) fn same_cell(
        &self,
        table: u64,
        record: u64,
        cell: u64,
        value: &Option<String>,
    ) -> bool {
        self.0
            .iter()
            .find(|(key, _, _)| *key == (table, record))
            .is_some_and(|(_, held, _)| match &held.change {
                Change::New(values) | Change::Edit { values, .. } => {
                    values.get(&cell) == Some(value)
                }
                _ => false,
            })
    }
    pub(crate) fn same_records(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .all(|(key, _, _)| other.contains(key.0, key.1))
    }
    pub(crate) fn validate(&self, owner: &mut NativeProvider) -> Result<(), DataError> {
        for ((table, _), held, _) in &self.0 {
            let action = match held.change {
                Change::New(_) => om_data::host::security::RecordAction::Create,
                Change::Edit { .. } => om_data::host::security::RecordAction::Edit,
                _ => return Err(unavailable("The retained record cannot be restored.")),
            };
            owner.require_action(*table, action)?;
        }
        Ok(())
    }
}

/// An open transaction.
#[derive(Clone)]
pub(crate) struct Transaction {
    txn: u32,
    kind: TransactionKind,
    /// References acquired by op88/op87 while this transaction is open.
    pub(crate) containers: BTreeMap<(u64, Vec<u8>), i32>,
    // Taken before this ordinary session does any work, never after refusal.
    pub(crate) before_record: Option<om_data::SubmissionMark>,
    pub(crate) upload_attempted: bool,
    pub(crate) single_root_start: Option<crate::scalar_recovery::RootStart>,
    pub(crate) root_acquisitions: BTreeMap<(u64, Vec<u8>), i32>,
    options: TransactionOptions,
    held: BTreeMap<(u64, u64), Held>,
    /// The held records in the order they were first changed.
    order: Vec<(u64, u64)>,
    /// The serial numbers generated on creation that each held new record
    /// took when New Record created it, by field (`hold_new`).
    serials: BTreeMap<(u64, u64), BTreeMap<u64, String>>,
    /// Ending this provider's part of it sends op65, which releases every
    /// lock under its id on the connection. `false` for a related file's
    /// part joined to another file's transaction ([`NativeProvider::begin_under`]),
    /// and for that transaction while its coordinator holds the end
    /// ([`NativeProvider::hold_transaction_end`]): the shared transaction
    /// ends after every participant has settled, in each file that held
    /// part of it (related_owner.rs, #1006).
    ends: bool,
    /// A record session's held records as its window's commit found them
    /// (`DataProvider::mark_record_session`).
    mark: Option<Mark>,
    /// A related file's part: the new records a refused commit staged
    /// here, set aside with their reserved ids, serial numbers and creation
    /// values when the window rolled the commit back
    /// ([`NativeProvider::park_new_records`]). The next write naming one
    /// holds it again; one not named is not committed, its id staying used.
    parked: BTreeMap<(u64, u64), (Held, BTreeMap<u64, String>)>,
}

/// A transaction set aside with everything it opened: a window's record
/// session while another window is current (`use_window`) or while it is
/// entered over another transaction (`enter_session`). Its related files'
/// providers' own open transactions are its parts and are set aside with
/// it, so that no other window's write reaches them (#892).
pub(crate) struct SetAside {
    transaction: Option<Transaction>,
    owner: crate::related_owner::OwnerTransaction,
    /// The related files' providers' transactions, each by a route to its
    /// file.
    parts: Vec<(crate::external::TableRoute, Transaction)>,
}

/// A window's record session a reconnect carries to the new connection
/// (#152): it held only new records, none of them submitted, and nothing
/// else ([`NativeProvider::carries`]). Copied from the lost session before
/// it closes ([`NativeProvider::take_carried`]), held again once the file
/// is open ([`NativeProvider::hold_carried`]).
pub(crate) struct Carried {
    window: u32,
    options: TransactionOptions,
    /// Each new record by (table, the lost connection's id), as held, with
    /// the serial numbers New Record gave it.
    records: Vec<CarriedRecord>,
}

/// A carried new record: by (table, the lost connection's id), as held,
/// with its serial numbers.
type CarriedRecord = ((u64, u64), Held, BTreeMap<u64, String>);

impl Carried {
    pub(crate) fn window(&self) -> u32 {
        self.window
    }

    /// A window's session, or the records kept for it, carried again.
    fn of(window: u32, transaction: &Transaction) -> Carried {
        let records = transaction
            .order
            .iter()
            .filter_map(|key| {
                let held = transaction.held.get(key)?.clone();
                let serials = transaction.serials.get(key).cloned().unwrap_or_default();
                // Its preview was the lost session's (preview.rs).
                Some((
                    *key,
                    Held {
                        preview: None,
                        ..held
                    },
                    serials,
                ))
            })
            .collect();
        Carried {
            window,
            options: transaction.options,
            records,
        }
    }

    /// Its records as a session on no connection, kept for its window
    /// ([`NativeProvider::hold_carried`]): reads show them, nothing writes
    /// to it, and it is never ended on a host.
    fn kept(self) -> Transaction {
        let mut held = BTreeMap::new();
        let mut order = Vec::new();
        let mut serials = BTreeMap::new();
        for (key, record, taken) in self.records {
            held.insert(key, record);
            order.push(key);
            serials.insert(key, taken);
        }
        Transaction {
            txn: 0,
            kind: TransactionKind::Record,
            containers: BTreeMap::new(),
            before_record: None,
            upload_attempted: false,
            single_root_start: None,
            root_acquisitions: BTreeMap::new(),
            options: self.options,
            held,
            order,
            serials,
            ends: false,
            mark: None,
            parked: BTreeMap::new(),
        }
    }
}

/// What a window's record session held when its commit began staging the
/// window's changes in it ([`NativeProvider::mark_session`]).
#[derive(Clone)]
struct Mark {
    held: BTreeMap<(u64, u64), Held>,
    order: Vec<(u64, u64)>,
    serials: BTreeMap<(u64, u64), BTreeMap<u64, String>>,
    /// No related writes were staged then (parked reservations may exist).
    owner_untouched: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Held {
    change: Change,
    /// A script step changed the record (`DataProvider::write_record`):
    /// Commit Transaction plans it with a script's rules
    /// ([`Entry::script`]), else as a user's edit.
    script: bool,
    /// What the operation that last changed it supplied for its formulas
    /// ([`WriteContext`]): its window's layout, privilege set and host
    /// clock, so that Commit Transaction computes each record as the
    /// window and step that changed it would, not as the last write did.
    /// The latest change's is kept (OpenMaker's choice: Commit computes the
    /// record's values once, over all its changes), except for a record
    /// written from a preview, which keeps the preview's (`hold`).
    context: WriteContext,
    /// The preview a new record was written from (preview.rs), used up
    /// once Commit Transaction submits.
    preview: Option<om_data::PreparedId>,
    /// A new record's clock and UUID and its values computed with them
    /// (`hold_new`): reads show them under its cells, and Commit
    /// Transaction computes it with the same clock and UUID.
    created: Option<crate::preview::Created>,
    /// The cells whose held value the user typed (`ScriptWrite::typed`):
    /// Commit Transaction checks them against a related-values list by a
    /// user's rules (#1074). A script's later write of one takes it out.
    typed: BTreeSet<u64>,
    /// Caller inputs before serials and generated creation values are merged.
    /// None means their provenance was not captured (for example an import).
    supplied: Option<BTreeSet<u64>>,
    input_layouts: BTreeSet<u32>,
}

impl Held {
    /// The change as reads see it: a new record's computed values
    /// (`created`) under its cells.
    fn shown(&self) -> Change {
        match (&self.change, &self.created) {
            (Change::New(cells), Some(created)) => {
                let mut values = created.values.clone();
                values.extend(cells.iter().map(|(cell, value)| (*cell, value.clone())));
                Change::New(values)
            }
            (change, _) => change.clone(),
        }
    }
}

/// Held values by cell: a field's id for its first repetition, `F * 256 +
/// n` for repetition n; stored text, `None` empty.
type Cells = BTreeMap<u64, Option<String>>;

/// How the open transaction holds a record, for a preview computing it as
/// Commit Transaction would (preview.rs, #1063), with the rules it would
/// compute it by (`entry`) and what its operation supplied (`context`).
pub(crate) enum HeldBasis {
    /// A new record: its cells without the serial numbers it took when it
    /// was held, those serial numbers, and its clock and UUID.
    New {
        cells: Cells,
        serials: BTreeMap<u64, String>,
        stamp: Option<crate::preview::Stamp>,
        context: WriteContext,
        entry: Entry,
        /// Written from a preview: a later write keeps its context.
        previewed: bool,
    },
    /// An existing record locked for an edit, with the cells held over it.
    Edit {
        values: Cells,
        context: WriteContext,
        entry: Entry,
    },
    /// An imported or deleted one, which a preview does not compute.
    Other,
}

#[derive(Clone, Debug)]
pub(crate) enum Change {
    /// A record op71 reserved, with the values set so far.
    New(Cells),
    /// An existing record, locked at modification count `mod_count`.
    Edit {
        mod_count: u64,
        values: Cells,
    },
    /// A row Import Records wrote: its cells as the import's rules left
    /// them when it was held (written as they are), and the script's edits
    /// since (planned over them with the transaction's rules).
    Imported {
        /// `None`: a record op71 reserved; else the existing record's
        /// modification count when it was locked.
        mod_count: Option<u64>,
        cells: EncodedChanges,
        later: Cells,
    },
    Deleted,
}

/// Cells by id as a write stores them: masked text, `None` to remove.
type EncodedChanges = BTreeMap<u64, Option<Vec<u8>>>;

/// What one record of a commit sends and expects back.
#[derive(Clone)]
struct Prepared {
    table: Table,
    record: u64,
    node: Option<OwnedNode>,
    entry: OwnedNode,
    expected: Option<(FieldChanges, u64)>,
    /// An imported row's cells and modification count, checked cell by
    /// cell when read back (repetitions too).
    cells: Option<(EncodedChanges, u64)>,
    /// The stored container values the commit replaces or deletes, released
    /// once it succeeded (containers.md 5.1).
    released: Vec<Vec<u8>>,
    /// References entering changed cells, to distinguish consumed uploads
    /// from superseded held values after a successful commit.
    acquired: Vec<Vec<u8>>,
}

/// Each table's records and commit entries as a submission sends them:
/// one op14 per table and one op70 listing them all, slot lists in key
/// order (`prepared` sorted by table and record).
fn wire_tables<'a>(
    prepared: impl IntoIterator<Item = &'a Prepared>,
) -> Vec<(u64, Vec<OwnedNode>, Vec<OwnedNode>)> {
    let mut tables: Vec<(u64, Vec<OwnedNode>, Vec<OwnedNode>)> = Vec::new();
    for p in prepared {
        let at = match tables.iter().position(|(t, _, _)| *t == p.table.id) {
            Some(at) => at,
            None => {
                tables.push((p.table.id, Vec::new(), Vec::new()));
                tables.len() - 1
            }
        };
        tables[at].1.extend(p.node.clone());
        tables[at].2.push(p.entry.clone());
    }
    tables
}

/// Build what submitting `prepared` would send and refuse what cannot go
/// in one request, while preparing: a save of several files then reverts
/// every file before any of them sends record data (#1006), rather than
/// fail at a later file's upload after an earlier file committed.
fn check_wire(prepared: Vec<Prepared>) -> Result<Vec<Prepared>, DataError> {
    let mut sorted: Vec<&Prepared> = prepared.iter().collect();
    sorted.sort_by_key(|p| (p.table.id, p.record));
    let tables = wire_tables(sorted);
    for (_, nodes, _) in &tables {
        if !nodes.is_empty() {
            om_session::session::upload_payload(nodes.clone())
                .map_err(|e| failure("upload the transaction's records", e))?;
        }
    }
    om_session::session::commit_list(
        tables
            .into_iter()
            .map(|(table, _, entries)| (key_bytes(table), entries))
            .collect(),
    )
    .map_err(|e| failure("commit the transaction", e))?;
    Ok(prepared)
}

/// A transaction's held records planned for its commit
/// ([`NativeProvider::prepare_commit`]). It owns the transaction, whose
/// locks and container references stay held until it is submitted or
/// abandoned.
///
/// A copy kept before it is submitted is what a save that may be partly
/// applied reads back and sends again (related_owner.rs, #1006): the values
/// it computed, never computed again. Only one of the copies settles its
/// container references.
#[derive(Clone)]
pub(crate) struct PreparedCommit {
    transaction: Transaction,
    prepared: Vec<Prepared>,
}

impl PreparedCommit {
    /// The transaction whose records it saves, as it holds them.
    pub(crate) fn transaction(&self) -> &Transaction {
        &self.transaction
    }

    /// One retained final record with its complete cells, including styles.
    /// Reading it never re-plans the commit or changes its observed version.
    pub(crate) fn written_record(
        &self,
        table: u64,
        record: u64,
    ) -> Option<Option<om_hbam::Record>> {
        self.prepared
            .iter()
            .find(|row| row.table.id == table && row.record == record)
            .map(|row| row.node.as_ref().map(|node| stored_record(record, node)))
    }

    /// Its records as the commit writes them, by its file's own table ids:
    /// each one's every cell, its computed values among them, and `None`
    /// for a deleted one. They are final once prepared: what another
    /// file's commit in the same save reads of them (#1167).
    pub(crate) fn written(
        &self,
    ) -> impl Iterator<Item = (u64, u64, Option<BTreeMap<u64, String>>)> + '_ {
        self.prepared.iter().map(|prepared| {
            let row = prepared
                .node
                .as_ref()
                .map(|node| text_values(&stored_record(prepared.record, node)));
            (prepared.table.id, prepared.record, row)
        })
    }
}

/// What reading back a sent commit's records shows, its outcome unknown
/// ([`NativeProvider::read_back`], #1006).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Readback {
    /// Every record as the commit wrote it.
    Applied,
    /// Every record as before the commit (an edit at the version it was
    /// locked at, a new record absent, a deleted one still there), under
    /// locks its transaction still holds: not applied.
    NotApplied,
    /// Neither, mixed, unreadable, or its locks released: why.
    Unresolved(String),
}

/// What submitting a prepared commit did ([`NativeProvider::submit_commit`]).
#[derive(Debug)]
pub(crate) enum CommitOutcome {
    /// op70 was not sent, so no record was committed: the error says why.
    NotSubmitted(DataError),
    /// Every record was committed and read back as written.
    Committed,
    /// Every op14 was sent and op70 was sent or attempted, then something
    /// failed (the statusless op70 reply, a readback, the connection):
    /// what the host kept is unknown.
    Ambiguous(DataError),
}

/// `cells` of `transaction`'s held new record `key` without the serial
/// numbers it took when it was created (`hold_new`), which the record's
/// rules enter again from the write's environment; and those serials. A
/// value set over a serial stays.
fn held_serials(
    transaction: Option<&Transaction>,
    key: (u64, u64),
    cells: &Cells,
) -> (Cells, BTreeMap<u64, String>) {
    let serials = transaction
        .and_then(|t| t.serials.get(&key))
        .cloned()
        .unwrap_or_default();
    let cells = cells
        .iter()
        .filter(|(cell, value)| value.is_none() || serials.get(cell) != value.as_ref())
        .map(|(cell, value)| (*cell, value.clone()))
        .collect();
    (cells, serials)
}

fn unavailable(message: &str) -> DataError {
    DataError::with_code(code::UNSUPPORTED, message)
}

impl Transaction {
    /// Open Transaction's "Skip data entry validation".
    pub(crate) fn skips_validation(&self) -> bool {
        self.options.skip_validation
    }

    /// Open Transaction's "Skip auto-enter options".
    pub(crate) fn skips_auto_enter(&self) -> bool {
        self.options.skip_auto_enter
    }
}

impl NativeProvider {
    pub(crate) fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    /// Whether the transaction holds `record` of `table` as a new record
    /// (`Some(true)`) or holds it otherwise (`Some(false)`).
    pub(crate) fn transaction_new(&self, table: u64, record: u64) -> Option<bool> {
        self.held(table, record).map(|change| {
            matches!(
                change,
                Change::New(_)
                    | Change::Imported {
                        mod_count: None,
                        ..
                    }
            )
        })
    }

    /// The records the open transaction holds as new ones
    /// ([`Self::transaction_new`]), and those the related files hold new
    /// for it on layouts of their tables (`OwnerTransaction::held_new`),
    /// as (table, record id).
    pub(crate) fn held_new_records(&self) -> Vec<(u64, i64)> {
        let own = self
            .transaction
            .iter()
            .flat_map(|t| t.held.iter())
            .filter(|(_, held)| {
                matches!(
                    held.change,
                    Change::New(_)
                        | Change::Imported {
                            mod_count: None,
                            ..
                        }
                )
            })
            .map(|(&key, _)| key);
        let routed = self
            .transaction
            .is_some()
            .then(|| self.owner_transaction.held_new())
            .into_iter()
            .flatten();
        own.chain(routed)
            .collect::<BTreeSet<(u64, u64)>>()
            .into_iter()
            .filter_map(|(table, record)| Some((table, i64::try_from(record).ok()?)))
            .collect()
    }

    /// Whether the open transaction holds a record of `table`: a new one
    /// op71 reserved (not yet in the host's record list), or one it edits.
    pub(crate) fn holds_records_of(&self, table: u64) -> bool {
        self.transaction
            .as_ref()
            .is_some_and(|t| t.held.keys().any(|(t, _)| *t == table))
    }

    /// The held change of `record` of `table`, if any.
    pub(crate) fn held(&self, table: u64, record: u64) -> Option<&Change> {
        self.transaction
            .as_ref()?
            .held
            .get(&(table, record))
            .map(|h| &h.change)
    }

    /// `all`, the host's record ids of `table`, with the transaction's new
    /// records and without its deleted ones.
    pub(crate) fn with_held(&self, table: u64, all: RidList) -> RidList {
        let mut all = all;
        let mut deleted = RidList::new();
        let held = self
            .shown_transactions()
            .flat_map(|transaction| &transaction.held);
        for ((t, record), held) in held {
            if *t != table {
                continue;
            }
            match held.change {
                Change::New(_)
                | Change::Imported {
                    mod_count: None, ..
                } => {
                    all.insert(*record);
                }
                Change::Deleted => {
                    deleted.insert(*record);
                }
                Change::Edit { .. } | Change::Imported { .. } => {}
            }
        }
        all.and_not(&deleted)
    }

    /// `all`, the host's record ids of imported table `table`, with the
    /// records the open transaction holds of it in its related file
    /// (`write_routed_related`, the window's record session's new record of
    /// a related file's layout among them): its new ones in, its deleted
    /// ones out.
    pub(crate) fn with_routed_held(&self, table: u64, all: RidList) -> RidList {
        let mut all = all;
        let mut deleted = RidList::new();
        let aside = self
            .record_sessions
            .get(&self.window)
            .into_iter()
            .flat_map(|session| session.owner.held_of(table));
        for (record, _, new, gone) in self.owner_transaction.held_of(table).chain(aside) {
            if gone {
                deleted.insert(record);
            } else if new {
                all.insert(record);
            }
        }
        all.and_not(&deleted)
    }

    /// Where `record` of imported table `table` is held as new by its
    /// related file: the owner's table id, for the open transaction (no
    /// part), or for the current window's record session set aside while
    /// another transaction is open (its part, set aside with it).
    fn routed_new(&self, table: u64, record: u64) -> Option<(u64, Option<&Transaction>)> {
        if let Some(wire) = Self::held_new_in(self.owner_transaction.held_of(table), record) {
            return Some((wire, None));
        }
        let session = self.record_sessions.get(&self.window)?;
        let wire = Self::held_new_in(session.owner.held_of(table), record)?;
        let route = self.external.route(table)?;
        let part = session
            .parts
            .iter()
            .find(|(known, _)| known.same_file(route))
            .map(|(_, part)| part)?;
        Some((wire, Some(part)))
    }

    /// The owner's table id of `record` among `held` (`held_of`) when it
    /// was made there and not deleted since.
    fn held_new_in(held: impl Iterator<Item = (u64, u64, bool, bool)>, record: u64) -> Option<u64> {
        let mut wire = None;
        for (held, owner_table, new, gone) in held {
            if held == record {
                if gone {
                    return None;
                }
                if new {
                    wire = Some(owner_table);
                }
            }
        }
        wire
    }

    /// Whether `record` of imported table `table` is a new record its
    /// related file holds for the open transaction: not on the host yet.
    pub(crate) fn routed_held_new(&self, table: u64, record: u64) -> bool {
        self.routed_new(table, record).is_some()
    }

    /// The cells of new record `record` of imported table `table` its
    /// related file holds for the open transaction, its computed values
    /// among them (`held_new_cells` there).
    pub(crate) fn routed_new_cells(&self, table: u64, record: u64) -> Option<Cells> {
        match self.routed_new(table, record)? {
            (wire, Some(part)) => match part.held.get(&(wire, record)).map(Held::shown) {
                Some(Change::New(cells)) => Some(cells),
                _ => None,
            },
            (wire, None) => {
                let route = self.external.route(table)?;
                self.external
                    .with_owner(route, |owner, _| Ok(owner.held_new_cells(wire, record)))
                    .ok()
                    .flatten()
            }
        }
    }

    /// [`Self::overlay_page`] for imported table `table`: the new records
    /// its related file holds for the open transaction filled in at their
    /// place among `ids`.
    pub(crate) fn overlay_routed(&self, table: &Table, ids: &[u64], page: &mut RecordPage) {
        if !ids.iter().any(|id| self.routed_held_new(table.id, *id)) {
            return;
        }
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            let Ok(signed) = i64::try_from(*id) else {
                continue;
            };
            let record = match self.routed_new_cells(table.id, *id) {
                Some(cells) => Some(held_record(table, signed, None, &cells)),
                None => page.records.iter().find(|r| r.id == signed).cloned(),
            };
            records.extend(record);
        }
        page.records = records;
    }

    /// A page's records with the transaction's values: held edits applied,
    /// held new records filled in at their place among `ids`.
    pub(crate) fn overlay_page(&self, table: &Table, ids: &[u64], page: &mut RecordPage) {
        // Their held records are apart: a record session locks its own.
        for transaction in self.shown_transactions() {
            Self::overlay_held(transaction, table, ids, page);
        }
    }

    fn overlay_held(transaction: &Transaction, table: &Table, ids: &[u64], page: &mut RecordPage) {
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            let Ok(signed) = i64::try_from(*id) else {
                continue;
            };
            let held = transaction.held.get(&(table.id, *id)).map(Held::shown);
            let base = page.records.iter().find(|r| r.id == signed).cloned();
            let record = match (held.as_ref(), base) {
                (Some(Change::New(values)), _) => Some(held_record(table, signed, None, values)),
                (Some(Change::Edit { values, .. }), Some(base)) => {
                    Some(held_record(table, signed, Some(base), values))
                }
                (
                    Some(Change::Imported {
                        mod_count,
                        cells,
                        later,
                    }),
                    base,
                ) => {
                    let base = base.filter(|_| mod_count.is_some());
                    let imported = held_cells(table, signed, base, cells);
                    Some(held_record(table, signed, Some(imported), later))
                }
                (Some(Change::Deleted), _) => None,
                (_, base) => base,
            };
            records.extend(record);
        }
        page.records = records;
    }

    /// Open a transaction of `kind` under a new id: a script's Open
    /// Transaction, or a window's commit of a record with its related
    /// records.
    pub(crate) fn begin_kind(
        &mut self,
        options: TransactionOptions,
        kind: TransactionKind,
    ) -> Result<(), DataError> {
        self.require_open()?;
        if self.transaction.is_some() {
            return Err(unavailable("A transaction is already open."));
        }
        let before = (kind == TransactionKind::Record
            && self
                .pending_assets
                .get(&self.window)
                .is_none_or(|p| p.values().all(|n| *n <= 0)))
        .then(|| om_data::DataProvider::submission_mark(self))
        .flatten();
        let txn = self.new_transaction();
        self.open_transaction(txn, kind, options, true);
        if let Some(t) = &mut self.transaction {
            t.before_record = before;
        }
        Ok(())
    }

    /// Join `shared`, another file's open transaction: its id, kind and
    /// options. Refused (3) unless this provider's session is a handle on
    /// that transaction's connection ([`om_session::Session::share`]):
    /// another connection, to the same host or not, numbers its own
    /// transactions, and the host keys locks by connection and id.
    /// Its locks are the shared transaction's, and its part of it ends
    /// without op65 (`Transaction::ends`).
    pub(crate) fn begin_under(&mut self, shared: &SharedTransaction) -> Result<(), DataError> {
        // No session at all is refused below too, with the same 3.
        if self.transaction.is_some() {
            return Err(unavailable("A transaction is already open."));
        }
        if !self.can_join(shared) {
            return Err(unavailable(
                "A transaction can be joined only on the connection it was opened on.",
            ));
        }
        self.open_transaction(shared.txn, shared.kind, shared.options, false);
        Ok(())
    }

    /// The id a joined transaction shares.
    pub(crate) fn shared_txn(shared: &SharedTransaction) -> u32 {
        shared.txn
    }

    /// Whether this provider's session is a handle on `shared`'s connection,
    /// where [`Self::begin_under`] can join it.
    pub(crate) fn can_join(&self, shared: &SharedTransaction) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.connection() == shared.connection)
    }

    /// Leave ending the open transaction to its coordinator, which ends it
    /// once ([`Self::end_transaction`]) after every file joined to it has
    /// settled: committing, abandoning or reverting it releases only what
    /// this provider acquired. Its id; `None` with none open.
    pub(crate) fn hold_transaction_end(&mut self) -> Option<u32> {
        let transaction = self.transaction.as_mut()?;
        transaction.ends = false;
        Some(transaction.txn)
    }

    fn open_transaction(
        &mut self,
        txn: u32,
        kind: TransactionKind,
        options: TransactionOptions,
        ends: bool,
    ) {
        self.transaction = Some(Transaction {
            txn,
            kind,
            containers: self.pending_assets.remove(&self.window).unwrap_or_default(),
            before_record: None,
            upload_attempted: false,
            single_root_start: None,
            root_acquisitions: BTreeMap::new(),
            options,
            held: BTreeMap::new(),
            order: Vec::new(),
            serials: BTreeMap::new(),
            ends,
            mark: None,
            parked: BTreeMap::new(),
        });
    }

    /// Whether the window's record session holds existing record `record`
    /// of `table` as it is edited, locked as its editing began
    /// (`prepare_root_record_edit`): `Some(true)` when it holds nothing
    /// else, of this file or a related one, so that nothing of it is on
    /// the host but that lock.
    pub(crate) fn session_edit(&self, table: u64, record: u64) -> Option<bool> {
        let t = self
            .transaction
            .as_ref()
            .filter(|t| t.kind == TransactionKind::Record)?;
        let held = t.held.get(&(table, record))?;
        if !matches!(held.change, Change::Edit { .. }) {
            return None;
        }
        Some(
            self.owner_transaction.alone()
                && t.mark.is_none()
                && t.containers.is_empty()
                && t.root_acquisitions.is_empty()
                && t.parked.is_empty()
                && t.held.len() == 1,
        )
    }

    /// The window's record session holds only record `record` of `table`,
    /// locked as its editing began and not changed since
    /// ([`Self::session_edit`]), and the window keeps no edit of it any
    /// more: the session goes, which lets the lock go.
    pub(crate) fn release_untouched_edit(&mut self, table: u64, record: u64) {
        let untouched = self.session_edit(table, record) == Some(true)
            && matches!(
                self.held(table, record),
                Some(Change::Edit { values, .. }) if values.is_empty()
            );
        if untouched {
            let _ = om_data::DataProvider::revert_record_session(self);
        }
    }

    /// Delete Record of the record the window's record session holds as it
    /// is edited, and nothing else ([`Self::session_edit`]): deleted under
    /// the lock its editing took, its edits going with the session, as
    /// FileMaker Pro deletes the record being edited. A delete the host
    /// refuses leaves no delete held for a later save: the session goes.
    pub(crate) fn delete_edited(&mut self, table: u64, record_id: i64) -> Result<(), DataError> {
        self.invalidate_calc_data();
        let deleted = self
            .hold_delete(table, record_id)
            .and_then(|()| om_data::DataProvider::commit_record_session(self, true));
        if deleted.is_err() && self.transaction_kind() == Some(TransactionKind::Record) {
            let _ = om_data::DataProvider::revert_record_session(self);
        }
        deleted
    }

    /// The open transaction's kind.
    pub(crate) fn transaction_kind(&self) -> Option<TransactionKind> {
        self.transaction.as_ref().map(|t| t.kind)
    }

    /// The open transaction as a related file's provider on this
    /// connection joins it ([`Self::begin_under`]); `None` with no
    /// transaction open or no connection.
    pub(crate) fn shared_transaction(&self) -> Option<SharedTransaction> {
        let transaction = self.transaction.as_ref()?;
        Some(SharedTransaction {
            connection: self.session.as_ref()?.connection(),
            txn: transaction.txn,
            kind: transaction.kind,
            options: transaction.options,
        })
    }

    /// The rules a held change is checked by when the transaction commits,
    /// as far as they are known before then (`commit_held` may also skip
    /// data entry validation).
    fn held_entry(&self) -> Entry {
        Entry {
            script: false,
            skip_data_entry_validation: self
                .transaction
                .as_ref()
                .is_some_and(|t| t.options.skip_validation),
            skip_auto_enter: self
                .transaction
                .as_ref()
                .is_some_and(|t| t.options.skip_auto_enter),
            keep_written: false,
        }
    }

    fn transaction_id(&self) -> Result<u32, DataError> {
        self.transaction
            .as_ref()
            .map(|t| t.txn)
            .ok_or_else(|| unavailable("No transaction is open."))
    }

    /// Hold `change` of `record` by an operation that supplied `context`; a
    /// record a script changed once stays planned as a script's.
    fn hold(
        &mut self,
        table: u64,
        record: u64,
        change: Change,
        script: bool,
        context: WriteContext,
    ) {
        if let Some(transaction) = &mut self.transaction {
            let before = transaction.held.get(&(table, record));
            let script = script || before.is_some_and(|h| h.script);
            // A record written from a preview keeps the context the preview
            // drew (preview.rs), as its clock and UUID: a later change gives
            // only a record number, when it gives one.
            let context = match before.filter(|h| h.preview.is_some()) {
                Some(previewed) => WriteContext {
                    record_number: context.record_number.or(previewed.context.record_number),
                    ..previewed.context.clone()
                },
                // A record made in the transaction keeps the place it was
                // made at, which its auto-entered Get ( RecordNumber ) was
                // given, through later writes that give none: a window's
                // commit or navigation forgets it, the record is still
                // the transaction's to create (#526).
                None => WriteContext {
                    record_number: context.record_number.or_else(|| {
                        before
                            .filter(|h| matches!(h.change, Change::New(_)))
                            .and_then(|h| h.context.record_number)
                    }),
                    ..context
                },
            };
            let held = Held {
                script,
                context,
                preview: before.and_then(|h| h.preview),
                created: before.and_then(|h| h.created.clone()),
                typed: before.map(|h| h.typed.clone()).unwrap_or_default(),
                supplied: match &change {
                    Change::Edit { values, .. } => Some(values.keys().copied().collect()),
                    _ => before.and_then(|h| h.supplied.clone()),
                },
                input_layouts: before.map(|h| h.input_layouts.clone()).unwrap_or_default(),
                change,
            };
            if transaction.held.insert((table, record), held).is_none() {
                transaction.order.push((table, record));
            }
        }
    }

    /// The changes `transaction` holds of records of `tables`, for
    /// [`Self::held_records`].
    fn held_of(transaction: &Transaction, tables: &BTreeSet<u64>) -> Vec<((u64, u64), Change)> {
        transaction
            .held
            .iter()
            .filter(|((table, _), _)| tables.contains(table))
            .map(|(key, held)| (*key, held.shown()))
            .collect()
    }

    /// The open transaction's held records of the related tables `plan`'s
    /// formulas read, over the stored ones ([`Self::held_records`]); `None`
    /// outside a transaction.
    fn held_overlay(
        &mut self,
        plan: &om_data::rules::RecordPlan,
    ) -> Result<Option<std::sync::Arc<HeldRecords>>, DataError> {
        self.held_overlay_with(Some(&plan.related_tables()))
    }

    /// The open transaction's held records of `tables` (all with `None`), as
    /// a write's reads see them ([`Self::held_records`]); `None` when there
    /// are none.
    pub(crate) fn held_overlay_with(
        &mut self,
        tables: Option<&BTreeSet<u64>>,
    ) -> Result<Option<std::sync::Arc<HeldRecords>>, DataError> {
        let held = match (&self.transaction, tables) {
            (Some(transaction), Some(tables)) => Self::held_of(transaction, tables),
            (Some(transaction), None) => transaction
                .held
                .iter()
                .map(|(key, held)| (*key, held.shown()))
                .collect(),
            (None, _) => Vec::new(),
        };
        self.held_records(held)
    }

    /// The records the open transaction holds and does not delete, in
    /// first-change order (the stable input order for commit planning).
    pub(crate) fn held_in_order(&self) -> Vec<(u64, u64)> {
        let Some(transaction) = self.transaction.as_ref() else {
            return Vec::new();
        };
        transaction
            .order
            .iter()
            .copied()
            .filter(|key| {
                transaction
                    .held
                    .get(key)
                    .is_some_and(|held| !matches!(held.change, Change::Deleted))
            })
            .collect()
    }

    /// How the open transaction holds record `key` ([`HeldBasis`]), `None`
    /// when it does not.
    pub(crate) fn held_basis(&self, key: (u64, u64)) -> Option<HeldBasis> {
        let transaction = self.transaction.as_ref()?;
        let held = transaction.held.get(&key)?;
        let entry = |new: bool| Entry {
            script: held.script,
            keep_written: held.script && new,
            ..self.held_entry()
        };
        Some(match &held.change {
            Change::New(cells) => {
                let (cells, serials) = held_serials(Some(transaction), key, cells);
                HeldBasis::New {
                    cells,
                    serials,
                    stamp: held.created.as_ref().map(|created| created.stamp.clone()),
                    context: held.context.clone(),
                    entry: entry(true),
                    previewed: held.preview.is_some(),
                }
            }
            Change::Edit { values, .. } => HeldBasis::Edit {
                values: values.clone(),
                context: held.context.clone(),
                entry: entry(false),
            },
            Change::Imported { .. } | Change::Deleted => HeldBasis::Other,
        })
    }

    /// The cells of record `record` of `table` the open transaction holds
    /// as new, its computed values among them (`Held::shown`); `None` for
    /// any other record.
    pub(crate) fn held_new_cells(&self, table: u64, record: u64) -> Option<Cells> {
        let held = self
            .shown_transactions()
            .find_map(|transaction| transaction.held.get(&(table, record)))?;
        match held.shown() {
            Change::New(cells) => Some(cells),
            _ => None,
        }
    }

    /// Whether the open transaction holds a record written from preview
    /// `id` (preview.rs).
    pub(crate) fn holds_preview(&self, id: om_data::PreparedId) -> bool {
        self.transaction.as_ref().is_some_and(|transaction| {
            transaction
                .held
                .values()
                .any(|held| held.preview == Some(id))
        })
    }

    /// The records `changes` hold, as a write's lookups and related reads
    /// see them (`HeldRecords`, #187): a new record's held cells (the
    /// serial numbers New Record took among them), an edited or imported
    /// one's stored record with what is held over it, a deleted one
    /// deleted. Nothing is committed; `None` when there are none.
    fn held_records(
        &mut self,
        changes: Vec<((u64, u64), Change)>,
    ) -> Result<Option<std::sync::Arc<HeldRecords>>, DataError> {
        self.held_records_over(None, changes)
    }

    /// [`Self::held_records`] over `base`, records of other tables held
    /// elsewhere (another participant's in the same save, #1167).
    fn held_records_over(
        &mut self,
        base: Option<&HeldRecords>,
        changes: Vec<((u64, u64), Change)>,
    ) -> Result<Option<std::sync::Arc<HeldRecords>>, DataError> {
        if changes.is_empty() {
            return Ok(base
                .filter(|base| !base.is_empty())
                .map(|base| std::sync::Arc::new(base.clone())));
        }
        let mut held = base.cloned().unwrap_or_default();
        for ((table, record), change) in changes {
            let (node, over) = match change {
                Change::Deleted => {
                    held.delete(table, record);
                    continue;
                }
                Change::New(values) => {
                    (OwnedNode::list(serial::encode(record), Vec::new()), values)
                }
                Change::Edit { values, .. } => match self.read_record(table, record)? {
                    Some(node) => (node, values),
                    None => {
                        held.delete(table, record);
                        continue;
                    }
                },
                Change::Imported {
                    mod_count,
                    cells,
                    later,
                } => {
                    let mut node = match mod_count {
                        None => OwnedNode::list(serial::encode(record), Vec::new()),
                        Some(_) => match self.read_record(table, record)? {
                            Some(node) => node,
                            None => {
                                held.delete(table, record);
                                continue;
                            }
                        },
                    };
                    apply_values(&mut node, cells);
                    (node, later)
                }
            };
            let mut row = text_values(&stored_record(record, &node));
            for (cell, value) in over {
                match value {
                    Some(text) => row.insert(cell, text),
                    None => row.remove(&cell),
                };
            }
            held.hold(table, record, row);
        }
        Ok(Some(std::sync::Arc::new(held)))
    }

    /// `prepared`, a record as the commit writes it, as the held row the
    /// records prepared after it read when a held record's formulas read its
    /// table: its computed values and serials, not its held cells.
    fn prepared_row(
        read_tables: Option<&BTreeSet<u64>>,
        prepared: Option<&Prepared>,
    ) -> Option<(u64, u64, BTreeMap<u64, String>)> {
        let prepared = prepared?;
        let node = prepared.node.as_ref()?;
        read_tables
            .is_none_or(|tables| tables.contains(&prepared.table.id))
            .then(|| {
                (
                    prepared.table.id,
                    prepared.record,
                    text_values(&stored_record(prepared.record, node)),
                )
            })
    }

    /// Preview only records whose validation can reach a related file. Held
    /// entries still defer validation to commit; this checks the actual values
    /// their combined changes would generate, without reserving ids or locks.
    fn preflight_held(
        &mut self,
        table: &Table,
        plan: &om_data::rules::RecordPlan,
        (record, prepared): (Option<i64>, Option<om_data::PreparedId>),
        cells: &Cells,
        mut entry: Entry,
        context: &WriteContext,
    ) -> Result<(), DataError> {
        let schema = self.calc_schema_cached()?;
        let fields: BTreeSet<_> = table
            .fields
            .iter()
            .filter(|field| {
                plan.value_list_field(field.id, entry)
                    .is_some_and(|(occurrence, _)| shows_related_file(&schema, occurrence))
            })
            .map(|field| field.id)
            .collect();
        if fields.is_empty() {
            return Ok(());
        }
        let mut existing = BTreeMap::new();
        let mut changes = Cells::new();
        let mut serials = BTreeMap::new();
        let mut stamp = None;
        let mut mode = WriteMode::Create;
        if let Some(id) = record {
            let id = u64::try_from(id).map_err(|_| DataError::new("Invalid record ID."))?;
            let held = self
                .transaction
                .as_ref()
                .and_then(|t| t.held.get(&(table.id, id)))
                .cloned();
            if let Some(held) = &held {
                entry.script |= held.script;
            }
            match held.map(|held| held.change) {
                Some(Change::Deleted) => {
                    return Err(DataError::with_code(
                        RECORD_MISSING,
                        "This record was deleted in the transaction.",
                    ))
                }
                Some(Change::New(values)) => {
                    (changes, serials) =
                        held_serials(self.transaction.as_ref(), (table.id, id), &values);
                    entry.keep_written = entry.script;
                }
                held => {
                    mode = WriteMode::Update;
                    let mut node = match &held {
                        Some(Change::Imported {
                            mod_count: None, ..
                        }) => OwnedNode::list(serial::encode(id), Vec::new()),
                        _ => self.read_record(table.id, id)?.ok_or_else(|| {
                            DataError::with_code(RECORD_MISSING, "This record no longer exists.")
                        })?,
                    };
                    match held {
                        Some(Change::Edit { values, .. }) => changes = values,
                        Some(Change::Imported { cells, later, .. }) => {
                            apply_values(&mut node, cells);
                            changes = later;
                        }
                        _ => {}
                    }
                    existing = text_values(&stored_record(id, &node));
                }
            }
        } else {
            entry.keep_written = entry.script;
            // What the commit computes it with: its preview's serial
            // numbers, clock and UUID.
            if let Some(id) = prepared {
                let fields = plan
                    .serials_for(WriteMode::Create, entry)
                    .into_iter()
                    .filter(|serial| !serial.on_commit)
                    .map(|serial| serial.field)
                    .collect();
                let reuse = self.preview_reuse(id, table.id, &fields)?;
                serials = reuse.serials;
                stamp = Some(reuse.stamp);
            }
        }
        changes.extend(cells.clone());
        let mut environment = RecordEnvironment {
            serials,
            held: self.held_overlay(plan)?,
            ..self.rule_environment(table.id, plan, context.clone())?
        };
        if let Some(stamp) = stamp {
            stamp.apply(&mut environment);
        }
        let generated =
            plan.preview_entry(mode, &existing, &changes, &environment, entry, &fields)?;
        self.refuse_related_lists(table, plan, entry, entered(&generated))
    }

    /// New Record inside the transaction: op71 reserves and locks the id.
    pub(crate) fn hold_create(
        &mut self,
        layout_id: u32,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
        let (table, plan) = self.record_plan(layout_id)?;
        let cells = Self::writable_values(&table, &plan, &values)?;
        let entry = self.held_entry();
        let context = self.write_context(&plan, Some(layout_id), CurrentSet::Account)?;
        self.preflight_held(&table, &plan, (None, None), &cells, entry, &context)?;
        self.hold_new(
            &table,
            &plan,
            entry,
            cells,
            false,
            (context, None, Some(layout_id)),
        )
    }

    /// A new record of `table` holding `cells`: op71 reserves and locks
    /// its id under the transaction. Its serial numbers generated on
    /// creation are taken now, as FileMaker Pro takes them right after
    /// op71 (write-path.md 1.1), and held with it: a script reads them
    /// before Commit Transaction. A record written from a preview
    /// (`prepared`, preview.rs) takes the serial numbers the preview
    /// reserved instead, and keeps its clock and UUID for the commit.
    fn hold_new(
        &mut self,
        table: &Table,
        plan: &om_data::rules::RecordPlan,
        entry: Entry,
        mut cells: Cells,
        script: bool,
        (context, prepared, layout): (WriteContext, Option<om_data::PreparedId>, Option<u32>),
    ) -> Result<Record, DataError> {
        self.begin_single_root_proof(table.id, cells.keys().copied().collect(), layout);
        self.retain_creation_input(table.id, &cells, prepared);
        let supplied = cells.keys().copied().collect();
        let txn = self.transaction_id()?;
        let on_creation: Vec<_> = plan
            .serials_for(WriteMode::Create, entry)
            .into_iter()
            .filter(|serial| !serial.on_commit)
            .collect();
        // Checked before op71 reserves an id.
        let reuse = match prepared {
            Some(id) => {
                let fields = on_creation.iter().map(|serial| serial.field).collect();
                Some((id, self.preview_reuse(id, table.id, &fields)?))
            }
            None => None,
        };
        if let Some((_, reuse)) = &reuse {
            self.retain_creation_preview(table.id, reuse);
        }
        let reply = self
            .session_mut()?
            .create_record(&key_bytes(table.id), txn)
            .map_err(|e| failure("create record", e))?;
        if reply.status != 0 {
            return Err(DataError::with_code(
                reply.status as i32,
                format!(
                    "The server refused to create a record (error {}).",
                    reply.status
                ),
            ));
        }
        let record = match reply.record.as_slice() {
            [key] => serial::decode(key),
            _ => None,
        }
        .ok_or_else(|| DataError::new("The server returned an invalid record ID."))?;
        self.retain_creation_reservation(table.id, record);
        let mut serials = reuse
            .as_ref()
            .map(|(_, reuse)| reuse.serials.clone())
            .unwrap_or_default();
        let missing = on_creation
            .into_iter()
            .filter(|serial| !serials.contains_key(&serial.field));
        let reserved = self.reserve_serials(table.id, missing)?;
        serials.extend(reserved);
        for (field, value) in &serials {
            cells.entry(*field).or_insert_with(|| Some(value.clone()));
        }
        // What the record shows until its commit: its auto-entered values
        // among them, which FileMaker Pro gives a new record when New
        // Record creates it.
        let created = self.created_values(
            (table.id, plan),
            Entry {
                keep_written: entry.script,
                ..entry
            },
            (&cells, &serials),
            context.clone(),
            reuse.as_ref().map(|(_, reuse)| &reuse.stamp),
        )?;
        if let Some(transaction) = &mut self.transaction {
            transaction.serials.insert((table.id, record), serials);
        }
        self.hold(table.id, record, Change::New(cells), script, context);
        let Some(held) = self
            .transaction
            .as_mut()
            .and_then(|transaction| transaction.held.get_mut(&(table.id, record)))
        else {
            return Err(unavailable("No transaction is open."));
        };
        held.supplied = Some(supplied);
        held.input_layouts.extend(layout);
        held.preview = reuse.map(|(id, _)| id);
        held.created = Some(created);
        let shown = match held.shown() {
            Change::New(values) => values,
            _ => Cells::new(),
        };
        let record = held_record(table, record as i64, None, &shown);
        self.bind_created_row(table.id, record.id as u64);
        self.remember_single_root();
        Ok(record)
    }

    /// A change to a record inside the transaction: the first locks it.
    pub(crate) fn hold_update(
        &mut self,
        layout_id: u32,
        record_id: i64,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
        let (table, plan) = self.record_plan(layout_id)?;
        let cells = Self::writable_values(&table, &plan, &values)?;
        let entry = self.held_entry();
        let context = self.write_context(&plan, Some(layout_id), CurrentSet::Account)?;
        self.preflight_held(
            &table,
            &plan,
            (Some(record_id), None),
            &cells,
            entry,
            &context,
        )?;
        self.hold_edit(&table, record_id, cells, false, (context, Some(layout_id)))
    }

    /// A change of `cells` of an existing record of `table` inside the
    /// transaction: the first change locks the record.
    fn hold_edit(
        &mut self,
        table: &Table,
        record_id: i64,
        values: Cells,
        script: bool,
        (context, layout): (WriteContext, Option<u32>),
    ) -> Result<Record, DataError> {
        let supplied: BTreeSet<_> = values.keys().copied().collect();
        let record = u64::try_from(record_id).map_err(|_| DataError::new("Invalid record ID."))?;
        let txn = self.transaction_id()?;
        let mut change = match self.held(table.id, record).cloned() {
            Some(Change::Deleted) => {
                return Err(DataError::with_code(
                    RECORD_MISSING,
                    "This record was deleted in the transaction.",
                ))
            }
            Some(change) => change,
            None => {
                let locked = self.lock_record(table.id, record, txn)?.ok_or_else(|| {
                    DataError::with_code(RECORD_MISSING, "This record no longer exists.")
                })?;
                // The version the window showed when the user began editing
                // the record (a retained one), else the last one read.
                if self
                    .observed_version((table.id, record))?
                    .is_some_and(|seen| seen != locked)
                {
                    return Err(DataError::with_code(
                        STALE_RECORD,
                        "Another client changed this record. Revert or refresh to load its \
                         changes before saving.",
                    ));
                }
                Change::Edit {
                    mod_count: locked,
                    values: Cells::new(),
                }
            }
        };
        match &mut change {
            Change::New(held)
            | Change::Edit { values: held, .. }
            | Change::Imported { later: held, .. } => held.extend(values),
            Change::Deleted => {}
        }
        // A new record's values computed again over its changed cells, as
        // the commit will: a stored calculation follows the typed inputs.
        let created = match &change {
            Change::New(cells) => self.created_again((table.id, record), cells, script)?,
            _ => None,
        };
        self.hold(table.id, record, change, script, context);
        if let Some(held) = self
            .transaction
            .as_mut()
            .and_then(|t| t.held.get_mut(&(table.id, record)))
        {
            if let Some(fields) = &mut held.supplied {
                fields.extend(supplied);
            }
            held.input_layouts.extend(layout);
        }
        if let Some(created) = created {
            if let Some(held) = self
                .transaction
                .as_mut()
                .and_then(|transaction| transaction.held.get_mut(&(table.id, record)))
            {
                held.created = Some(created);
            }
        }
        let stored = match self.held(table.id, record) {
            Some(
                Change::Edit { .. }
                | Change::Imported {
                    mod_count: Some(_), ..
                },
            ) => self.read_record(table.id, record)?.map(|node| Record {
                id: record_id,
                // As a page shows it (#783).
                values: super::page_values(&stored_record(record, &node), table),
            }),
            _ => None,
        };
        let shown = self
            .transaction
            .as_ref()
            .and_then(|transaction| transaction.held.get(&(table.id, record)))
            .map(Held::shown);
        let shown = match shown.as_ref() {
            Some(Change::New(values) | Change::Edit { values, .. }) => {
                held_record(table, record_id, stored, values)
            }
            Some(Change::Imported { cells, later, .. }) => {
                let imported = held_cells(table, record_id, stored, cells);
                held_record(table, record_id, Some(imported), later)
            }
            _ => held_record(table, record_id, stored, &Cells::new()),
        };
        self.remember_single_root();
        Ok(shown)
    }

    /// Held new record `key`'s values (`Held::created`) computed again
    /// over `cells`, with the clock, UUID and serial numbers it was created
    /// with; `None` for a record held without them.
    fn created_again(
        &mut self,
        key: (u64, u64),
        cells: &Cells,
        script: bool,
    ) -> Result<Option<crate::preview::Created>, DataError> {
        let Some(transaction) = &self.transaction else {
            return Ok(None);
        };
        let Some(held) = transaction.held.get(&key) else {
            return Ok(None);
        };
        let Some(created) = &held.created else {
            return Ok(None);
        };
        let (stamp, context) = (created.stamp.clone(), held.context.clone());
        let script = script || held.script;
        let serials = transaction.serials.get(&key).cloned().unwrap_or_default();
        let (_, plan) = self.table_plan(key.0)?;
        let entry = Entry {
            script,
            keep_written: script,
            ..self.held_entry()
        };
        self.created_values(
            (key.0, &plan),
            entry,
            (cells, &serials),
            context,
            Some(&stamp),
        )
        .map(Some)
    }

    /// A script's write inside the transaction
    /// (`DataProvider::write_record`): held as an edit or a new record of
    /// base table `table`, planned with a script's rules at Commit
    /// Transaction. `origin` as for `script_native_record`.
    pub(crate) fn hold_script(
        &mut self,
        table: u64,
        record: Option<i64>,
        (cells, typed): (&Cells, &BTreeSet<u64>),
        origin: Origin,
    ) -> Result<Record, DataError> {
        self.refuse_imported_table(table)?;
        let table = self
            .catalog
            .table(table)
            .ok_or_else(|| DataError::with_code(error::FIELD_MISSING, "The table is missing."))?
            .clone();
        // Held script edits use the definitions already read at open, as
        // before; preflight must not download fields for every Set Field.
        let plan = self.plan_table(&table, None)?;
        let entry = Entry {
            script: true,
            ..self.held_entry()
        };
        let prepared = origin.prepared;
        let layout = origin.layout;
        // A new record a refused commit parked, named by its next write.
        if let Some(id) = record.and_then(|id| u64::try_from(id).ok()) {
            self.unpark(table.id, id);
        }
        let context = self.origin_context(&plan, origin)?;
        self.preflight_held(&table, &plan, (record, prepared), cells, entry, &context)?;
        let held = match record {
            None => self.hold_new(
                &table,
                &plan,
                entry,
                cells.clone(),
                true,
                (context, prepared, layout),
            ),
            Some(id) => self.hold_edit(&table, id, cells.clone(), true, (context, layout)),
        }?;
        // Which of its held cells the user typed, as this write leaves them.
        let key = (table.id, u64::try_from(held.id).unwrap_or_default());
        if let Some(held) = self.transaction.as_mut().and_then(|t| t.held.get_mut(&key)) {
            for cell in cells.keys() {
                match typed.contains(cell) {
                    true => held.typed.insert(*cell),
                    false => held.typed.remove(cell),
                };
            }
        }
        Ok(held)
    }

    /// Delete Record inside the transaction: a held new record is dropped
    /// (its id stays used), another one is locked and marked deleted.
    pub(crate) fn hold_delete(&mut self, table: u64, record_id: i64) -> Result<(), DataError> {
        self.leave_single_root_recovery()?;
        let record = u64::try_from(record_id).map_err(|_| DataError::new("Invalid record ID."))?;
        let txn = self.transaction_id()?;
        match self.held(table, record) {
            Some(
                Change::New(_)
                | Change::Imported {
                    mod_count: None, ..
                },
            ) => {
                if let Some(transaction) = &mut self.transaction {
                    transaction.held.remove(&(table, record));
                    transaction.order.retain(|key| *key != (table, record));
                }
                return Ok(());
            }
            Some(Change::Deleted) => {
                return Err(DataError::with_code(
                    RECORD_MISSING,
                    "This record was deleted in the transaction.",
                ))
            }
            Some(Change::Edit { .. } | Change::Imported { .. }) => {}
            None => {
                if self.lock_record(table, record, txn)?.is_none() {
                    return Err(DataError::with_code(
                        RECORD_MISSING,
                        "This record no longer exists.",
                    ));
                }
            }
        }
        self.hold(
            table,
            record,
            Change::Deleted,
            false,
            WriteContext::default(),
        );
        Ok(())
    }

    /// Import Records' row inside the transaction
    /// (`DataProvider::import_record`): the import's rules
    /// ([`Entry::import`]) apply when the row is held, as OpenMaker Server
    /// stages an imported row, and the transaction keeps the cells they
    /// left; Commit Transaction writes them as they are. A new record's id
    /// is reserved and locked with op71, an existing record is locked. A
    /// record the script changed earlier in the transaction gets the
    /// import's rules for those changes too (OpenMaker's choice, as on the
    /// server); a later row of the import over the same record starts from
    /// what the earlier one left.
    pub(crate) fn hold_import(
        &mut self,
        layout_id: u32,
        record: Option<i64>,
        cells: &BTreeMap<u64, Option<String>>,
        auto_enter: bool,
    ) -> Result<u64, DataError> {
        let (table, plan) = self.record_plan(layout_id)?;
        let txn = self.transaction_id()?;
        let entry = Entry::import(auto_enter);
        let reserve = record.is_none();
        let lock = record.is_some_and(|id| {
            u64::try_from(id)
                .ok()
                .is_some_and(|id| self.held(table.id, id).is_none())
        });
        // An import may be a script's step: its set is not known.
        let context = self.write_context(&plan, Some(layout_id), CurrentSet::Step(None))?;
        let reads = plan.related_tables();
        // Its rules run now: a related record the transaction changed is
        // computed only at Commit, so it would be read without its computed
        // values. Refused. An imported row's own cells are final, unless the
        // script changed it since (its `later` edits are computed at Commit).
        if self.transaction.as_ref().is_some_and(|transaction| {
            transaction.held.iter().any(|((held_table, _), held)| {
                reads.contains(held_table)
                    && match &held.change {
                        Change::New(_) | Change::Edit { .. } => true,
                        Change::Imported { later, .. } => !later.is_empty(),
                        Change::Deleted => false,
                    }
            })
        }) {
            return Err(DataError::with_code(
                error::UNAVAILABLE,
                "An imported row's calculation reads a related record the transaction \
                 changed, which OpenMaker computes only at Commit. No values were changed.",
            ));
        }
        let changes = self
            .transaction
            .as_ref()
            .map(|transaction| Self::held_of(transaction, &reads))
            .unwrap_or_default();
        let environment = RecordEnvironment {
            held: self.held_records(changes)?,
            ..self.rule_environment(table.id, &plan, context.clone())?
        };
        // The script's held edits of the record go under the row's.
        let mut changes = BTreeMap::new();
        let (record, mod_count, mode, existing, mut held_cells, later) = match record {
            None => {
                let (none, empty) = (BTreeMap::new(), EncodedChanges::new());
                (0, None, WriteMode::Create, none, empty, Cells::new())
            }
            Some(id) => {
                let record = u64::try_from(id).map_err(|_| DataError::new("Invalid record ID."))?;
                match self.held(table.id, record).cloned() {
                    Some(Change::Deleted) => {
                        return Err(DataError::with_code(
                            RECORD_MISSING,
                            "This record was deleted in the transaction.",
                        ))
                    }
                    Some(Change::Imported {
                        mod_count,
                        cells,
                        later,
                    }) => {
                        let mut node = match mod_count {
                            Some(_) => self.read_record(table.id, record)?.ok_or_else(|| {
                                DataError::with_code(
                                    RECORD_MISSING,
                                    "This record no longer exists.",
                                )
                            })?,
                            None => OwnedNode::list(serial::encode(record), Vec::new()),
                        };
                        apply_values(&mut node, cells.clone());
                        let existing = text_values(&stored_record(record, &node));
                        (record, mod_count, WriteMode::Update, existing, cells, later)
                    }
                    Some(Change::New(values)) => {
                        changes = values;
                        let (none, empty) = (BTreeMap::new(), EncodedChanges::new());
                        (record, None, WriteMode::Create, none, empty, Cells::new())
                    }
                    Some(Change::Edit { mod_count, values }) => {
                        changes = values;
                        let node = self.read_record(table.id, record)?.ok_or_else(|| {
                            DataError::with_code(RECORD_MISSING, "This record no longer exists.")
                        })?;
                        let existing = text_values(&stored_record(record, &node));
                        let empty = EncodedChanges::new();
                        (
                            record,
                            Some(mod_count),
                            WriteMode::Update,
                            existing,
                            empty,
                            Cells::new(),
                        )
                    }
                    None => {
                        let node = self.read_record(table.id, record)?.ok_or_else(|| {
                            DataError::with_code(RECORD_MISSING, "This record no longer exists.")
                        })?;
                        let stored = stored_record(record, &node);
                        let locked = super::records::mod_count(&stored)?;
                        let empty = EncodedChanges::new();
                        (
                            record,
                            Some(locked),
                            WriteMode::Update,
                            text_values(&stored),
                            empty,
                            Cells::new(),
                        )
                    }
                }
            }
        };
        changes.extend(cells.iter().map(|(cell, value)| (*cell, value.clone())));
        // A row not reserved yet is 0 here. A row imported before is
        // computed over what that import left, not its stored row: it names
        // no record, and answers no self-join (#1035).
        let imported_before = matches!(self.held(table.id, record), Some(Change::Imported { .. }));
        let written = match mode {
            _ if imported_before => Written::Unknown,
            WriteMode::Create => Written::New((record != 0).then_some(record)),
            _ => Written::Stored(record),
        };
        let prepared = self.prepare_record(
            &plan,
            (mode, entry, written),
            &existing,
            &changes,
            &environment,
        )?;
        self.refuse_related_lists(&table, &plan, entry, entered(&prepared.values))?;
        let record = if reserve {
            let reply = self
                .session_mut()?
                .create_record(&key_bytes(table.id), txn)
                .map_err(|e| failure("create record", e))?;
            if reply.status != 0 {
                return Err(DataError::with_code(
                    reply.status as i32,
                    format!(
                        "The server refused to create a record (error {}).",
                        reply.status
                    ),
                ));
            }
            match reply.record.as_slice() {
                [key] => serial::decode(key),
                _ => None,
            }
            .ok_or_else(|| DataError::new("The server returned an invalid record ID."))?
        } else {
            if lock {
                let locked = self.lock_record(table.id, record, txn)?.ok_or_else(|| {
                    DataError::with_code(RECORD_MISSING, "This record no longer exists.")
                })?;
                if Some(locked) != mod_count {
                    return Err(DataError::with_code(
                        STALE_RECORD,
                        "The record changed while locking. Refresh before editing again.",
                    ));
                }
            }
            record
        };
        self.check_unique(
            &table,
            plan.unique_fields_for(entry),
            &prepared.values,
            record,
            txn,
        )?;
        self.check_lookups(&table, &plan, entry, &prepared.values, record)?;
        let held: BTreeSet<(u64, u64)> = self
            .transaction
            .as_ref()
            .map(|t| t.held.keys().copied().collect())
            .unwrap_or_default();
        self.check_related_lists(
            &table,
            &plan,
            (entry, &BTreeSet::new()),
            (&existing, &prepared.values),
            (written, crate::related_lists::Holding::uncomputed(&held)),
        )?;
        held_cells.extend(prepared.values);
        self.hold(
            table.id,
            record,
            Change::Imported {
                mod_count,
                cells: held_cells,
                later,
            },
            false,
            context,
        );
        Ok(record)
    }

    /// What Commit Transaction writes for an imported row (see
    /// [`Self::hold_import`]): its cells as the import's rules left them,
    /// then the script's later edits with the transaction's rules over
    /// them; unique values among the held records noted in `unique`.
    fn commit_imported(
        &mut self,
        table: &Table,
        plan: &om_data::rules::RecordPlan,
        record: u64,
        (mod_count, cells, later, typed): (Option<u64>, &EncodedChanges, &Cells, &BTreeSet<u64>),
        (entry, environment, txn): (Entry, &om_data::rules::RecordEnvironment, u32),
        unique: &mut UniqueHeld,
    ) -> Result<Prepared, DataError> {
        let record_key = serial::encode(record);
        let mut node = match mod_count {
            None => OwnedNode::list(record_key.clone(), Vec::new()),
            Some(expected) => {
                let node = self.read_record(table.id, record)?.ok_or_else(|| {
                    DataError::with_code(RECORD_MISSING, "A changed record no longer exists.")
                })?;
                if super::records::mod_count(&stored_record(record, &node))? != expected {
                    return Err(DataError::with_code(
                        STALE_RECORD,
                        "A changed record was modified by another client.",
                    ));
                }
                node
            }
        };
        apply_values(&mut node, cells.clone());
        let file_language = self.file_language()?;
        note_unique(
            unique,
            table,
            plan.unique_fields_for(Entry::import(true)),
            cells,
            (record, file_language),
        )?;
        let mut written = cells.clone();
        if !later.is_empty() {
            let existing = text_values(&stored_record(record, &node));
            // Over what the import left, not the stored row (#1035).
            let ready = self.prepare_record(
                plan,
                (WriteMode::Update, entry, Written::Unknown),
                &existing,
                later,
                environment,
            )?;
            self.refuse_related_lists(table, plan, entry, entered(&ready.values))?;
            let fields = plan.unique_fields_for(entry);
            note_unique(
                unique,
                table,
                fields.clone(),
                &ready.values,
                (record, file_language),
            )?;
            self.check_unique(table, fields, &ready.values, record, txn)?;
            self.check_lookups(table, plan, entry, &ready.values, record)?;
            // Over what the import left, not the stored row: not named, so a
            // later edit's typed value checked against such a list is
            // refused (3) rather than taken unchecked (#1074).
            self.check_related_lists(
                table,
                plan,
                (entry, typed),
                (&existing, &ready.values),
                (Written::Unknown, crate::related_lists::Holding::none()),
            )?;
            apply_values(&mut node, ready.values.clone());
            written.extend(ready.values);
        }
        let (commit, next) = match mod_count {
            None => (OwnedNode::leaf(record_key, vec![write::COMMIT_NEW]), 0),
            Some(mod_count) => {
                let next = mod_count
                    .checked_add(1)
                    .ok_or_else(|| DataError::new("A record's modification count is exhausted."))?;
                node.set_child(OwnedNode::leaf(key_bytes(MOD_COUNT), mod_count_value(next)));
                let fields: Vec<u64> = written.keys().copied().collect();
                let changed = super::records::changed_fields(table, &fields)
                    .into_iter()
                    .map(|field| OwnedNode::leaf(key_bytes(field), Vec::new()))
                    .collect();
                (OwnedNode::list(record_key, changed), next)
            }
        };
        Ok(Prepared {
            table: table.clone(),
            record,
            node: Some(node),
            entry: commit,
            expected: None,
            cells: Some((written, next)),
            released: Vec::new(),
            acquired: Vec::new(),
        })
    }

    /// Revert Transaction: release everything the transaction holds.
    pub(crate) fn revert(&mut self) -> Result<(), DataError> {
        let transaction = self
            .transaction
            .take()
            .ok_or_else(|| unavailable("No transaction is open."))?;
        self.end_taken(&transaction, false);
        Ok(())
    }

    /// End a transaction taken from `self.transaction`: op65 releases its
    /// locks, unless another file's transaction holds them too or its
    /// coordinator ends it (`Transaction::ends`). Before op70 (`submitted`
    /// false) no stored record can own its container references, so they
    /// are released. Once it was sent, even a failed readback may have
    /// stored them: they are kept for recovery rather than delete data.
    fn end_taken(&mut self, transaction: &Transaction, submitted: bool) {
        if transaction.ends {
            self.end_transaction(transaction.txn);
        }
        if !submitted {
            self.release_transaction_containers(transaction, &BTreeMap::new());
            return;
        }
        // Its new records may be stored with their previews' serial
        // numbers (preview.rs); a commit not submitted leaves them for the
        // next attempt.
        for held in transaction.held.values() {
            if let Some(id) = held.preview {
                self.use_up_preview(id);
            }
        }
    }

    fn release_transaction_containers(
        &mut self,
        transaction: &Transaction,
        used: &BTreeMap<(u64, Vec<u8>), i32>,
    ) {
        for ((table, member), taken) in &transaction.containers {
            let keep = used
                .get(&(*table, member.clone()))
                .copied()
                .unwrap_or_default();
            for _ in keep..*taken {
                self.release_members(*table, vec![member.clone()]);
            }
        }
    }

    /// The window's direct record writes (`DataProvider::create_record`,
    /// `update_record`, `delete_record`, the user's own saves) are refused
    /// while its record session holds a new record: held with it, they
    /// would go with a revert, and nothing would commit them. The window's
    /// commit writes the session's records through `write_record` (#892).
    pub(crate) fn refuse_beside_record_session(&self) -> Result<(), DataError> {
        let holds_new = self.transaction.as_ref().is_some_and(|transaction| {
            transaction.kind == TransactionKind::Record
                && transaction
                    .held
                    .values()
                    .any(|held| matches!(held.change, Change::New(_)))
        });
        if holds_new {
            return Err(unavailable(
                "This window's new record is not saved yet: save or revert it first.",
            ));
        }
        Ok(())
    }

    /// [`DataProvider::enter_record_session`].
    pub(crate) fn enter_session(&mut self) -> bool {
        if self.entered_over.is_some() || self.transaction_kind() == Some(TransactionKind::Record) {
            return false;
        }
        let Some(session) = self.record_sessions.remove(&self.window) else {
            return false;
        };
        let over = self.set_aside();
        self.put_back(session);
        self.entered_over = Some(over);
        self.invalidate_calc_data();
        true
    }

    /// [`DataProvider::leave_record_session`].
    pub(crate) fn leave_session(&mut self) {
        let Some(over) = self.entered_over.take() else {
            return;
        };
        if self.transaction_kind() == Some(TransactionKind::Record) {
            let session = self.set_aside();
            self.record_sessions.insert(self.window, session);
        }
        self.put_back(over);
        self.invalidate_calc_data();
    }

    /// Set the open transaction aside with its related files' parts: their
    /// providers' open transactions, taken out of them (`SetAside`).
    pub(crate) fn set_aside(&mut self) -> SetAside {
        self.remember_asset_drafts();
        let transaction = self.transaction.take();
        let owner = std::mem::take(&mut self.owner_transaction);
        let mut parts: Vec<(crate::external::TableRoute, Transaction)> = Vec::new();
        for table in owner.owner_tables() {
            let Some(route) = self.external.route(*table) else {
                continue;
            };
            if parts.iter().any(|(known, _)| known.same_file(route)) {
                continue;
            }
            let part = self
                .external
                .with_owner(route, |provider, _| Ok(provider.transaction.take()));
            if let Ok(Some(part)) = part {
                parts.push((route, part));
            }
        }
        SetAside {
            transaction,
            owner,
            parts,
        }
    }

    /// Make `aside` the open transaction again, its parts back in their
    /// providers. The place is empty: what was open is set aside first. A
    /// provider found holding another transaction keeps it, the part's
    /// locks left to the connection's end, rather than lose either.
    pub(crate) fn put_back(&mut self, aside: SetAside) {
        for (route, part) in aside.parts {
            let _ = self.external.with_owner(route, |provider, _| {
                if provider.transaction.is_none() {
                    provider.transaction = Some(part);
                } else {
                    log::warn!("a related file's transaction was open where a part went back");
                }
                Ok(())
            });
        }
        self.transaction = aside.transaction;
        self.owner_transaction = aside.owner;
    }

    /// The transactions whose held records the current window's reads
    /// show: the open one, the window's own record session while it is
    /// set aside for another (`DataProvider::record_session_held`), and
    /// this file's part of the window's save while it is unsettled, so
    /// that the frozen window still shows what it would save. Writes go
    /// only to the open one.
    fn shown_transactions(&self) -> impl Iterator<Item = &Transaction> {
        let aside = self
            .record_sessions
            .get(&self.window)
            .and_then(|session| session.transaction.as_ref());
        self.transaction
            .iter()
            .chain(aside)
            .chain(self.unsettled_shown())
            .chain(self.unrecreated_shown())
    }

    /// Put the current window's record session back in place when no
    /// transaction is open: on a switch to the window, and once a script's
    /// transaction it was set aside for ends.
    pub(crate) fn resume_window_session(&mut self) {
        if self.transaction.is_none() {
            if let Some(session) = self.record_sessions.remove(&self.window) {
                self.put_back(session);
            }
        }
    }

    /// End another window's record session as Revert ends it: put it in
    /// place for `revert_with_owner`, then put the current one back.
    pub(crate) fn end_parked_session(&mut self, session: SetAside) {
        let current = self.set_aside();
        self.put_back(session);
        let _ = self.revert_with_owner();
        self.put_back(current);
    }

    /// Whether a reconnect carries this record session (#152): it holds
    /// new records only, and nothing a commit or another file took part
    /// in. Nothing of it was submitted: a record session's records go to
    /// the host only with its commit (op71 reserves an id, which changes no
    /// record), a commit that may have sent data takes its transaction out
    /// (related_owner.rs), and one refused before any is rolled back to
    /// its mark. Left out, as OpenMaker cannot yet carry them:
    /// - a commit begun (`mark`): what it staged is not the window's;
    /// - container references, whose owner on the host is not checked;
    /// - a related file's part (`owner`, `parts`), on its own connection.
    fn carries(
        transaction: &Transaction,
        owner: &crate::related_owner::OwnerTransaction,
        parts: bool,
    ) -> bool {
        transaction.kind == TransactionKind::Record
            && transaction.mark.is_none()
            && transaction.containers.is_empty()
            && !transaction.upload_attempted
            && owner.alone()
            && !parts
            && !transaction.held.is_empty()
            && transaction
                .held
                .values()
                .all(|held| matches!(held.change, Change::New(_)))
    }

    /// The window's record sessions a reconnect carries now, each with its
    /// window, open or set aside.
    fn carried_sessions(&self) -> Vec<(u32, &Transaction)> {
        let open = self
            .transaction
            .as_ref()
            .filter(|transaction| Self::carries(transaction, &self.owner_transaction, false))
            .map(|transaction| (self.window, transaction));
        let aside = self.record_sessions.iter().filter_map(|(window, aside)| {
            let transaction = aside.transaction.as_ref()?;
            Self::carries(transaction, &aside.owner, !aside.parts.is_empty())
                .then_some((*window, transaction))
        });
        open.into_iter().chain(aside).collect()
    }

    /// [`DataProvider::carried_record_sessions`]: those copied from a lost
    /// session before a reopen that failed, those kept after a reopen
    /// could not hold them again, and those it holds now.
    pub(crate) fn carried_windows(&self) -> Vec<u32> {
        let mut windows: BTreeSet<u32> = self.carried.iter().map(|c| c.window).collect();
        windows.extend(self.unrecreated.keys().copied());
        windows.extend(
            self.carried_sessions()
                .into_iter()
                .map(|(window, _)| window),
        );
        if let Some(window) = self.scalar_carry_window() {
            windows.insert(window);
        }
        windows.into_iter().collect()
    }

    /// Copy the record sessions a reconnect carries out of the lost
    /// session, which closing then reverts, with those kept before. A
    /// window already carried keeps that copy.
    pub(crate) fn take_carried(&mut self) -> Vec<Carried> {
        let mut carried = std::mem::take(&mut self.carried);
        for (window, kept) in std::mem::take(&mut self.unrecreated) {
            if !carried.iter().any(|c| c.window == window) {
                carried.push(Carried::of(window, &kept));
            }
        }
        for (window, transaction) in self.carried_sessions() {
            if !carried.iter().any(|c| c.window == window) {
                carried.push(Carried::of(window, transaction));
            }
        }
        carried
    }

    /// The current window's kept new records, the ones a reconnect could
    /// not hold again ([`DataProvider::take_recreated_records`]).
    pub(crate) fn unrecreated_shown(&self) -> Option<&Transaction> {
        self.unrecreated.get(&self.window)
    }

    /// [`DataProvider::recreate_record`].
    pub(crate) fn recreate_kept(&mut self) -> Result<(), DataError> {
        let Some(kept) = self.unrecreated.remove(&self.window) else {
            return Err(unavailable("This window has no new record to make again."));
        };
        let carried = Carried::of(self.window, &kept);
        self.recreated = self.hold_carried(vec![carried]);
        self.invalidate_calc_data();
        match self.recreated.iter().find_map(|r| r.new.as_ref().err()) {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    /// [`DataProvider::discard_unrecreated`].
    pub(crate) fn discard_kept(&mut self) -> Result<(), DataError> {
        if self.unrecreated.remove(&self.window).is_none() {
            return Err(unavailable("This window has no new record to discard."));
        }
        self.invalidate_calc_data();
        Ok(())
    }

    /// Hold each carried session's new records again on the file's new
    /// connection, in a record session of its window: each under an id
    /// op71 reserves now, with its cells, clock, UUID, computed values and
    /// serial numbers as they were. Nothing is computed again and no serial
    /// number taken again. The window's found set shows each by its new id.
    /// What became of each ([`DataProvider::take_recreated_records`]).
    ///
    /// A window's records are held again all together or not at all: when
    /// one cannot be (its session not begun, op71 refused, an invalid id),
    /// the session begun for them ends (the ids op71 reserved stay used)
    /// and they are kept as they were, unchanged and under the lost
    /// connection's ids ([`Carried::kept`]): reads show them and the
    /// window takes no write until it tries again or discards them
    /// ([`DataProvider::recreate_record`], `discard_unrecreated`). Nothing
    /// is sent again by itself.
    pub(crate) fn hold_carried(&mut self, carried: Vec<Carried>) -> Vec<om_data::RecreatedRecord> {
        let front = self.window;
        let mut recreated = Vec::new();
        for carried in carried {
            om_data::DataProvider::use_window(self, carried.window);
            let mut held = Vec::new();
            let mut failure = self
                .begin_kind(carried.options, TransactionKind::Record)
                .err();
            if failure.is_none() {
                self.owner_transaction =
                    crate::related_owner::OwnerTransaction::opened(carried.options);
                for ((table, old), record, serials) in &carried.records {
                    match self.hold_again(*table, record.clone(), serials.clone()) {
                        Ok(new) => held.push((*table, *old, new)),
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    }
                }
            }
            match failure {
                None => {
                    for (table, old, new) in held {
                        if let Some(found) = self.found.get_mut(&table) {
                            found.renumber(old, new);
                        }
                        recreated.push(om_data::RecreatedRecord {
                            window: carried.window,
                            table,
                            old,
                            new: Ok(new),
                        });
                    }
                }
                Some(error) => {
                    if self.transaction_kind() == Some(TransactionKind::Record) {
                        let _ = self.revert();
                    }
                    for ((table, old), _, _) in &carried.records {
                        recreated.push(om_data::RecreatedRecord {
                            window: carried.window,
                            table: *table,
                            old: *old,
                            new: Err(error.clone()),
                        });
                    }
                    self.unrecreated.insert(carried.window, carried.kept());
                }
            }
        }
        om_data::DataProvider::use_window(self, front);
        recreated
    }

    /// Reserve a new record of `table` with op71 in the open record session
    /// and hold `held` there, with `serials`: its id.
    fn hold_again(
        &mut self,
        table: u64,
        held: Held,
        serials: BTreeMap<u64, String>,
    ) -> Result<u64, DataError> {
        let txn = self.transaction_id()?;
        let reply = self
            .session_mut()?
            .create_record(&key_bytes(table), txn)
            .map_err(|e| failure("create record", e))?;
        if reply.status != 0 {
            return Err(DataError::with_code(
                reply.status as i32,
                format!(
                    "The server refused to create a record (error {}).",
                    reply.status
                ),
            ));
        }
        let record = match reply.record.as_slice() {
            [key] => serial::decode(key),
            _ => None,
        }
        .ok_or_else(|| DataError::new("The server returned an invalid record ID."))?;
        let transaction = self
            .transaction
            .as_mut()
            .ok_or_else(|| unavailable("No transaction is open."))?;
        transaction.serials.insert((table, record), serials);
        if transaction.held.insert((table, record), held).is_none() {
            transaction.order.push((table, record));
        }
        Ok(record)
    }

    /// [`DataProvider::mark_record_session`]: keep what the open record
    /// session holds now.
    pub(crate) fn mark_session(&mut self) {
        let owner_untouched = self.owner_transaction.unstaged();
        if let Some(transaction) = self
            .transaction
            .as_mut()
            .filter(|transaction| transaction.kind == TransactionKind::Record)
        {
            transaction.mark = Some(Mark {
                held: transaction.held.clone(),
                order: transaction.order.clone(),
                serials: transaction.serials.clone(),
                owner_untouched,
            });
        }
    }

    /// Set the open transaction's held new records aside (`parked`), and
    /// let go of everything else it holds: a related file's part of a
    /// window's commit refused before any record data (#1095). The
    /// transaction stays open, so the ids op71 reserved stay reserved for
    /// the commit's next attempt; the other records it locked stay locked
    /// until it ends. Whether anything was set aside.
    pub(crate) fn park_new_records(&mut self) -> bool {
        let Some(transaction) = &mut self.transaction else {
            return false;
        };
        for key in std::mem::take(&mut transaction.order) {
            let Some(held) = transaction.held.remove(&key) else {
                continue;
            };
            let serials = transaction.serials.remove(&key).unwrap_or_default();
            if matches!(held.change, Change::New(_)) {
                transaction.parked.insert(key, (held, serials));
            }
        }
        transaction.held.clear();
        transaction.serials.clear();
        self.invalidate_calc_data();
        !self
            .transaction
            .as_ref()
            .is_some_and(|t| t.parked.is_empty())
    }

    /// Hold parked record `record` of `table` again, as it was
    /// ([`Self::park_new_records`]): a write names it.
    fn unpark(&mut self, table: u64, record: u64) {
        let Some(transaction) = &mut self.transaction else {
            return;
        };
        if let Some((held, serials)) = transaction.parked.remove(&(table, record)) {
            transaction.held.insert((table, record), held);
            transaction.order.push((table, record));
            transaction.serials.insert((table, record), serials);
        }
    }

    /// Whether `record` of `table` is a new record the open transaction
    /// holds or has parked: reserved, not on the host.
    pub(crate) fn reserved_new(&self, table: u64, record: u64) -> bool {
        self.transaction.as_ref().is_some_and(|transaction| {
            transaction.parked.contains_key(&(table, record))
                || matches!(
                    transaction.held.get(&(table, record)).map(|h| &h.change),
                    Some(Change::New(_))
                )
        })
    }

    /// Closing another window must detach the receipt's actual parked owner,
    /// not the transaction that happens to be current.
    pub(crate) fn detach_parked_assets(
        &mut self,
        window: u32,
        owner: Option<u64>,
        assets: &[super::asset_recovery::Asset],
    ) {
        let route = owner.and_then(|table| self.external.route(table));
        let Some(session) = self.record_sessions.get_mut(&window) else {
            return;
        };
        let transaction = match route {
            Some(route) => session
                .parts
                .iter_mut()
                .find(|(r, _)| r.same_file(route))
                .map(|(_, t)| t),
            None if owner.is_none() => session.transaction.as_mut(),
            None => None,
        };
        if let Some(t) = transaction {
            for asset in assets {
                let (key, owned) = asset.ownership();
                if let Some(count) = t.containers.get_mut(&key) {
                    *count = (*count - owned).max(0);
                }
            }
        }
    }

    /// Remove live cleanup authority while retaining every acknowledged count,
    /// including acquisitions whose immutable fingerprint could not be read.
    pub(crate) fn detach_attachment_counts(
        &mut self,
        window: u32,
        owner: Option<u64>,
    ) -> BTreeMap<(u64, Vec<u8>), i32> {
        let route = owner.and_then(|table| self.external.route(table));
        let mut counts = BTreeMap::new();
        if let Some(session) = self.record_sessions.get_mut(&window) {
            let transaction = match route {
                Some(route) => session
                    .parts
                    .iter_mut()
                    .find(|(r, _)| r.same_file(route))
                    .map(|(_, t)| t),
                None if owner.is_none() => session.transaction.as_mut(),
                None => None,
            };
            if let Some(t) = transaction {
                counts = std::mem::take(&mut t.containers);
            }
        }
        if self.window == window && self.transaction_kind() == Some(TransactionKind::Record) {
            let live = match route {
                Some(route) => self
                    .external
                    .with_owner(route, |owner, _| {
                        Ok(owner
                            .transaction
                            .as_mut()
                            .map(|t| std::mem::take(&mut t.containers))
                            .unwrap_or_default())
                    })
                    .unwrap_or_default(),
                None if owner.is_none() => self
                    .transaction
                    .as_mut()
                    .map(|t| std::mem::take(&mut t.containers))
                    .unwrap_or_default(),
                None => BTreeMap::new(),
            };
            for (key, count) in live {
                *counts.entry(key).or_default() += count;
            }
        }
        counts
    }

    pub(crate) fn detach_single_root_assets(&mut self, window: u32) {
        if let Some(t) = self
            .record_sessions
            .get_mut(&window)
            .and_then(|s| s.transaction.as_mut())
        {
            t.containers.clear();
        }
        if self.window == window && self.transaction_kind() == Some(TransactionKind::Record) {
            self.detach_assets();
        }
    }

    /// Every member named by held values, including staged existing edits.
    pub(crate) fn held_container_members(&self) -> BTreeSet<(u64, Vec<u8>)> {
        let mut members = BTreeSet::new();
        if let Some(t) = &self.transaction {
            for ((table, _), held) in t
                .held
                .iter()
                .chain(t.parked.iter().map(|(k, (h, _))| (k, h)))
            {
                if let Some(schema) = self.catalog.table(*table) {
                    let values = match held.shown() {
                        Change::New(v) | Change::Edit { values: v, .. } => v,
                        _ => continue,
                    };
                    let values = values
                        .into_iter()
                        .filter_map(|(k, v)| v.map(|v| (k, v)))
                        .collect();
                    members.extend(
                        super::containers::references(schema, &values, None)
                            .into_iter()
                            .map(|m| (*table, m)),
                    );
                }
            }
        }
        members
    }

    pub(crate) fn has_scalar_creations(&self) -> bool {
        self.transaction.as_ref().is_some_and(|t| {
            t.kind == TransactionKind::Record
                && (!t.parked.is_empty()
                    || t.held.values().any(|h| matches!(h.change, Change::New(_))))
        })
    }

    /// Capture caller permissions before rollback drops existing edits. Parked
    /// creations still carry their original input provenance and layout checks.
    pub(crate) fn scalar_footprint(&self) -> Option<scalar_recovery::Footprint> {
        let t = self.transaction.as_ref()?;
        if t.kind != TransactionKind::Record {
            return None;
        }
        let mut needs = scalar_recovery::Footprint::default();
        for (&(table, _), held) in t
            .held
            .iter()
            .chain(t.parked.iter().map(|(k, (h, _))| (k, h)))
        {
            let create = match held.change {
                Change::New(_) => true,
                Change::Edit { .. } => false,
                _ => return None,
            };
            let fields = held.supplied.as_ref()?;
            let definition = self.catalog.table(table)?;
            let fields = fields
                .iter()
                .map(|cell| {
                    definition
                        .field(*cell)
                        .or_else(|| definition.field(*cell >> 8))
                        .map(|f| f.id)
                })
                .collect::<Option<BTreeSet<_>>>()?;
            needs.write(table, create, fields, &held.input_layouts);
        }
        Some(needs)
    }

    pub(crate) fn scalar_edit_versions(&self) -> Option<Vec<(u64, u64, u64)>> {
        let t = self.transaction.as_ref()?;
        if t.kind != TransactionKind::Record {
            return None;
        }
        let mut versions = Vec::new();
        for (&(table, id), held) in &t.held {
            match held.change {
                Change::New(_) => {}
                Change::Edit { mod_count, .. } => versions.push((table, id, mod_count)),
                _ => return None,
            }
        }
        Some(versions)
    }

    pub(crate) fn scalar_records(&self) -> Option<ScalarRecords> {
        if !self.owner_transaction.alone() {
            return None;
        }
        self.mixed_row_records()
            .or_else(|| self.existing_root_records())
            .or_else(|| self.scalar_creation_records(false))
    }

    /// Root's new records stay held when a refused save rolls back to its
    /// mark; related new records are parked. Both carry the same issued state.
    pub(crate) fn scalar_root_records(&self) -> Option<ScalarRecords> {
        self.mixed_row_records()
            .or_else(|| self.existing_root_records())
            .or_else(|| self.scalar_creation_records(true))
    }

    fn existing_root_records(&self) -> Option<ScalarRecords> {
        let t = self.transaction.as_ref()?;
        let original = t.single_root_start.as_ref()?.existing.as_ref()?;
        let key = (original.table, original.record);
        if t.kind != TransactionKind::Record || t.held.len() != 1 || !t.parked.is_empty() {
            return None;
        }
        let held = t.held.get(&key)?;
        if !matches!(held.change, Change::Edit { mod_count, .. } if mod_count == original.version)
            || held.created.is_some()
        {
            return None;
        }
        Some(ScalarRecords(vec![(key, held.clone(), BTreeMap::new())]))
    }

    pub(crate) fn bind_created_row(&mut self, table: u64, record: u64) {
        if let Some(start) = self
            .transaction
            .as_mut()
            .and_then(|t| t.single_root_start.as_mut())
        {
            if let Some(token) = start.rows.creating {
                if matches!(start.rows.members.get(&token), Some(scalar_recovery::mixed::RowReceipt::Creating { table: t, .. }) if *t == table)
                {
                    start.rows.members.insert(
                        token,
                        scalar_recovery::mixed::RowReceipt::New { table, record },
                    );
                    start.rows.creating = None;
                }
                return;
            }
            if matches!(start.row, Some(scalar_recovery::mixed::RowReceipt::Creating { table: t, .. }) if t == table)
            {
                start.row = Some(scalar_recovery::mixed::RowReceipt::New { table, record });
            }
        }
    }

    fn mixed_row_records(&self) -> Option<ScalarRecords> {
        let t = self.transaction.as_ref()?;
        let start = t.single_root_start.as_ref()?;
        if !start.rows.members.is_empty() {
            if t.kind != TransactionKind::Record
                || start.rows.creating.is_some()
                || t.held.len() + t.parked.len() != start.rows.members.len()
            {
                return None;
            }
            let mut rows = Vec::new();
            for row in start.rows.members.values() {
                let key = row.key()?;
                let (held, serials) = if let Some(held) = t.held.get(&key) {
                    (
                        held.clone(),
                        if row.new_row() {
                            t.serials.get(&key)?.clone()
                        } else {
                            BTreeMap::new()
                        },
                    )
                } else {
                    t.parked.get(&key)?.clone()
                };
                match row {
                    scalar_recovery::mixed::RowReceipt::Existing(original) => {
                        if !matches!(held.change,Change::Edit { mod_count,.. } if mod_count == original.version)
                            || held.created.is_some()
                        {
                            return None;
                        }
                    }
                    scalar_recovery::mixed::RowReceipt::New { .. } => {
                        if !matches!(held.change, Change::New(_)) || held.created.is_none() {
                            return None;
                        }
                    }
                    _ => return None,
                }
                rows.push((key, held, serials));
            }
            let rows = ScalarRecords(rows);
            return start.rows.covers(&rows).then_some(rows);
        }
        let row = start.row.as_ref()?;
        let key = row.key()?;
        if t.kind != TransactionKind::Record || t.held.len() + t.parked.len() != 1 {
            return None;
        }
        if let scalar_recovery::mixed::RowReceipt::Existing(_) = row {
            return self.existing_root_records();
        }
        let (held, serials) = if let Some(held) = t.held.get(&key) {
            (held.clone(), t.serials.get(&key)?.clone())
        } else {
            t.parked.get(&key)?.clone()
        };
        if !matches!(held.change, Change::New(_)) || held.created.is_none() {
            return None;
        }
        Some(ScalarRecords(vec![(key, held, serials)]))
    }

    pub(crate) fn mixed_row_shape(&self, participant: bool, retaining: bool) -> bool {
        self.transaction.as_ref().is_some_and(|t| {
            t.kind == TransactionKind::Record
                && (participant || t.ends)
                && t.options == TransactionOptions::default()
                && (t.mark.is_none() || retaining)
                && t.single_root_start.as_ref().is_some_and(|s| {
                    s.participant == participant && (s.row.is_some() || !s.rows.members.is_empty())
                })
                && self.mixed_row_records().is_some()
                && (!participant || self.owner_transaction.alone())
        })
    }

    pub(crate) fn prepare_mixed_part(
        &mut self,
        start: scalar_recovery::RootStart,
        fields: BTreeSet<u64>,
    ) -> Result<(), DataError> {
        let t = self
            .transaction
            .as_ref()
            .ok_or_else(|| unavailable("No participant session is open."))?;
        let extending = t
            .single_root_start
            .as_ref()
            .is_some_and(|s| !s.rows.members.is_empty());
        let proved = extending && self.mixed_row_records().is_some();
        if t.kind != TransactionKind::Record
            || t.options != TransactionOptions::default()
            || t.mark.is_some()
            || if extending {
                !proved
            } else {
                !t.held.is_empty()
                    || !t.parked.is_empty()
                    || !t.containers.is_empty()
                    || t.single_root_start.is_some()
            }
            || !self.owner_transaction.alone()
            || self.scalar_carry.is_some()
            || self.asset_settlement.is_some()
        {
            return Err(unavailable(
                "The mixed participant has unproved prior work.",
            ));
        }
        let existing = start.existing.clone();
        self.transaction
            .as_mut()
            .ok_or_else(|| unavailable("No participant session is open."))?
            .single_root_start = Some(start);
        if let Some(original) = &existing {
            let (table, record, version) = (original.table, original.record, original.version);
            let txn = self.transaction_id()?;
            if self.lock_record(table, record, txn)? != Some(version) {
                return Err(DataError::with_code(
                    306,
                    "The accepted related record changed before entry.",
                ));
            }
            self.pinned
                .insert((self.window, table, record), Some(version));
            self.hold(
                table,
                record,
                Change::Edit {
                    mod_count: version,
                    values: Cells::new(),
                },
                false,
                WriteContext::default(),
            );
            if let Some(held) = self
                .transaction
                .as_mut()
                .and_then(|t| t.held.get_mut(&(table, record)))
            {
                held.supplied = Some(fields);
            }
        }
        Ok(())
    }

    /// Capture the accepted existing row before either scalar entry or Insert.
    /// Failure to prove recovery is not a new restriction on ordinary editing.
    pub(crate) fn prepare_root_record_edit(
        &mut self,
        request: &om_data::RecordEditRequest,
    ) -> Result<om_data::EditPreparation, DataError> {
        use om_data::EditPreparation::{Prepared, Unavailable};
        let baseline = request.baseline;
        let Some(version) = baseline.version else {
            return Ok(Unavailable);
        };
        let Ok(record) = u64::try_from(baseline.record) else {
            return Ok(Unavailable);
        };
        if request.cells.is_empty() && request.fields.is_empty()
            || self
                .transaction_kind()
                .is_some_and(|k| k != TransactionKind::Record)
            || self.asset_settlement.is_some()
            || self.entered_over.is_some()
        {
            return Ok(Unavailable);
        }
        // Another window's record session, or a reconnect's carried or
        // unsettled work, leaves an edit no recovery proof; the record is
        // locked all the same.
        let provable = self.scalar_session_alone();
        self.refuse_while_unsettled()?;
        let Ok((table, _)) = self.record_plan(baseline.layout) else {
            return Ok(Unavailable);
        };
        // Another file's table is written through its owner, which locks
        // the record only to commit it.
        if self.external.route(table.id).is_some() {
            return Ok(Unavailable);
        }
        if self
            .unprepared_root_edits
            .contains(&(self.window, table.id, baseline.record))
        {
            return Ok(Unavailable);
        }
        let Some(mut fields) = request
            .cells
            .iter()
            .map(|cell| {
                table
                    .field(*cell)
                    .or_else(|| table.field(*cell >> 8))
                    .map(|f| f.id)
            })
            .collect::<Option<BTreeSet<_>>>()
        else {
            return Ok(Unavailable);
        };
        for name in &request.fields {
            let Some(field) = table.field_by_name(name) else {
                return Ok(Unavailable);
            };
            fields.insert(field.id);
        }
        if fields.iter().any(|id| {
            table.field(*id).is_none_or(|f| {
                f.kind != om_hbam::FieldKind::Normal
                    || f.decoded_options()
                        .is_none_or(|o| o.options & 0x1_0000 != 0)
            })
        }) {
            return Ok(Unavailable);
        }
        if let Some(mut start) = self
            .transaction
            .as_ref()
            .and_then(|t| t.single_root_start.clone())
        {
            let Some(original) = &start.existing else {
                return Ok(Unavailable);
            };
            if (original.table, original.record) != (table.id, record) {
                return Ok(Unavailable);
            }
            if original.version != version {
                return Err(DataError::with_code(
                    306,
                    "The accepted record version changed before entry.",
                ));
            }
            if !start.extend_record_edit(self, table.id, fields, baseline.layout) {
                return Ok(Unavailable);
            }
            start.retain_record_edit(self)?;
            return Ok(Prepared);
        }
        if !self.owner_transaction.alone()
            || self.scalar_carry.is_some()
            || self.transaction.is_some() && self.single_root_baseline().is_none()
            || self
                .pending_assets
                .get(&self.window)
                .is_some_and(|p| p.values().any(|n| *n > 0))
        {
            return Ok(Unavailable);
        }
        if self
            .pinned
            .get(&(self.window, table.id, record))
            .is_some_and(|v| *v != Some(version))
        {
            return Err(DataError::with_code(
                306,
                "The accepted editing version changed before entry.",
            ));
        }
        let opened = self.transaction.is_none();
        if opened {
            om_data::DataProvider::begin_record_commit(self)?;
        }
        let prepared = (|| {
            let mark = self
                .single_root_baseline()
                .ok_or_else(|| unavailable("No before-work record baseline."))?;
            // What a reconnect can recover of the edit, where it can be
            // proved (the account's set, the file's identity); the record
            // is locked either way, as FileMaker Pro locks it.
            let mut start = match self.external.root_identity().cloned().filter(|_| provable) {
                Some(identity) => self.existing_edit_start_at(
                    &table,
                    scalar_recovery::ExistingAttachment {
                        record,
                        version,
                        field: None,
                    },
                    Some(baseline.layout),
                    mark,
                    identity,
                    false,
                )?,
                None => None,
            };
            if let Some(proved) = &mut start {
                if proved.extend_record_edit(self, table.id, fields, baseline.layout) {
                    proved.row = proved
                        .existing
                        .clone()
                        .map(scalar_recovery::mixed::RowReceipt::Existing);
                } else {
                    start = None;
                }
            }
            if self.lock_record(table.id, record, self.transaction_id()?)? != Some(version) {
                return Err(DataError::with_code(
                    306,
                    "The record changed before entry.",
                ));
            }
            self.pinned
                .insert((self.window, table.id, record), Some(version));
            self.hold(
                table.id,
                record,
                Change::Edit {
                    mod_count: version,
                    values: Cells::new(),
                },
                false,
                WriteContext::default(),
            );
            let t = self
                .transaction
                .as_mut()
                .ok_or_else(|| unavailable("No record session is open."))?;
            t.held
                .get_mut(&(table.id, record))
                .ok_or_else(|| unavailable("The accepted record is not held."))?
                .input_layouts
                .insert(baseline.layout);
            t.single_root_start = start;
            Ok(Prepared)
        })();
        if opened && !matches!(prepared, Ok(Prepared)) {
            let _ = om_data::DataProvider::revert_record_session(self);
        }
        prepared
    }

    pub(crate) fn prepare_existing_attachment(
        &mut self,
        layout: u32,
        record: i64,
        version: Option<u64>,
        field: &str,
    ) -> Result<bool, DataError> {
        let (table, _) = self.record_plan(layout)?;
        if self
            .unprepared_root_edits
            .contains(&(self.window, table.id, record))
        {
            return Ok(false);
        }
        // Related-first groups must complete their explicit pending Root
        // acquisition before scalar-first footprint extension can apply.
        if let Some(prepared) = self.prepare_awaiting_root_field(layout, record, version, field)? {
            return Ok(prepared);
        }
        if let Some(prepared) =
            self.extend_existing_root_attachment(layout, record, version, field)?
        {
            return Ok(prepared);
        }
        self.prepare_attachment_root(layout, record, version, Some(field))
    }

    pub(crate) fn prepare_attachment_root(
        &mut self,
        layout: u32,
        record: i64,
        version: Option<u64>,
        field: Option<&str>,
    ) -> Result<bool, DataError> {
        self.refuse_while_unsettled()?;
        self.require_writable()?;
        self.require_layout_records(layout)?;
        // Explicit transactions and previously begun work keep their existing
        // semantics; they cannot acquire a before-work recovery receipt here.
        if (self.transaction.is_some()
            && !(field.is_none() && self.single_root_baseline().is_some()))
            || self
                .transaction
                .as_ref()
                .is_some_and(|t| t.mark.is_some() || t.upload_attempted)
            || !self.owner_transaction.alone()
            || !self.scalar_session_alone()
            || self.scalar_carry.is_some()
            || self.asset_settlement.is_some()
            || self
                .pending_assets
                .get(&self.window)
                .is_some_and(|p| p.values().any(|n| *n > 0))
        {
            return Ok(false);
        }
        let (table, _) = self.record_plan(layout)?;
        if self
            .unprepared_root_edits
            .contains(&(self.window, table.id, record))
        {
            return Ok(false);
        }
        let field = match field {
            Some(name) => {
                let field = table
                    .fields
                    .iter()
                    .find(|f| f.name == name)
                    .ok_or_else(|| unavailable("The attachment field is unavailable."))?;
                if field.data_type != om_hbam::DataType::Container
                    || field.kind != om_hbam::FieldKind::Normal
                    || field
                        .decoded_options()
                        .is_none_or(|o| o.options & 0x1_0000 != 0)
                {
                    return Ok(false);
                }
                Some(field.id)
            }
            None => None,
        };
        let record = u64::try_from(record).map_err(|_| unavailable("Invalid record ID."))?;
        if self.external.route(table.id).is_some() {
            return Ok(false);
        }
        let version = version.ok_or_else(|| DataError::with_code(306, "The accepted record version is unavailable. Check the record before inserting an attachment."))?;
        if self
            .pinned
            .get(&(self.window, table.id, record))
            .is_some_and(|v| *v != Some(version))
        {
            return Err(DataError::with_code(
                306,
                "The record's accepted editing version changed.",
            ));
        }
        if self.transaction.is_none() {
            om_data::DataProvider::begin_record_commit(self)?;
        }
        let prepared = (|| {
            let Some(mut start) =
                self.existing_attachment_start(&table, record, version, field, layout)?
            else {
                return Ok(false);
            };
            start.row = Some(scalar_recovery::mixed::RowReceipt::Existing(
                start
                    .existing
                    .clone()
                    .ok_or_else(|| unavailable("The original record proof is missing."))?,
            ));
            let locked = self.lock_record(table.id, record, self.transaction_id()?)?;
            if locked != Some(version) {
                return Err(DataError::with_code(
                    306,
                    "The record changed before attachment entry.",
                ));
            }
            self.pinned
                .insert((self.window, table.id, record), Some(version));
            self.hold(
                table.id,
                record,
                Change::Edit {
                    mod_count: version,
                    values: Cells::new(),
                },
                false,
                WriteContext::default(),
            );
            let t = self
                .transaction
                .as_mut()
                .ok_or_else(|| unavailable("No record session is open."))?;
            let held = t
                .held
                .get_mut(&(table.id, record))
                .ok_or_else(|| unavailable("The prepared record is unavailable."))?;
            held.supplied = Some(field.into_iter().collect());
            held.input_layouts.insert(layout);
            t.single_root_start = Some(start);
            Ok(true)
        })();
        if !matches!(prepared, Ok(true)) {
            let _ = om_data::DataProvider::revert_record_session(self);
        }
        prepared
    }

    /// An empty freshly joined ordinary participant captures its own accepted
    /// existing row before any acquisition. Root retains the original mark.
    pub(crate) fn prepare_attachment_part(
        &mut self,
        wire: u64,
        record: u64,
        version: u64,
        field: u64,
        mark: om_data::SubmissionMark,
        identity: om_data::file_refs::FileIdentity,
    ) -> Result<scalar_recovery::RootStart, DataError> {
        let t = self
            .transaction
            .as_ref()
            .ok_or_else(|| unavailable("No participant session is open."))?;
        if t.kind != TransactionKind::Record
            || t.options != TransactionOptions::default()
            || t.mark.is_some()
            || !t.held.is_empty()
            || !t.parked.is_empty()
            || !t.containers.is_empty()
            || !self.owner_transaction.alone()
            || self.scalar_carry.is_some()
            || self.asset_settlement.is_some()
            || self
                .pinned
                .get(&(self.window, wire, record))
                .is_some_and(|v| *v != Some(version))
        {
            return Err(unavailable(
                "The attachment participant has unproved prior work.",
            ));
        }
        let (table, _) = self.table_plan(wire)?;
        let definition = table
            .field(field)
            .ok_or_else(|| unavailable("The attachment field is unavailable."))?;
        if definition.data_type != om_hbam::DataType::Container
            || definition.kind != om_hbam::FieldKind::Normal
            || definition
                .decoded_options()
                .is_none_or(|o| o.options & 0x1_0000 != 0)
        {
            return Err(unavailable(
                "The attachment field is not a stored container.",
            ));
        }
        let mut start = self
            .existing_attachment_start_at(
                &table,
                scalar_recovery::ExistingAttachment {
                    record,
                    version,
                    field: Some(field),
                },
                None,
                mark,
                identity,
            )?
            .ok_or_else(|| unavailable("The attachment participant cannot be proved."))?;
        if self.lock_record(wire, record, self.transaction_id()?)? != Some(version) {
            return Err(DataError::with_code(
                306,
                "The related attachment record changed before entry.",
            ));
        }
        self.pinned
            .insert((self.window, wire, record), Some(version));
        self.hold(
            wire,
            record,
            Change::Edit {
                mod_count: version,
                values: Cells::new(),
            },
            false,
            WriteContext::default(),
        );
        let t = self
            .transaction
            .as_mut()
            .ok_or_else(|| unavailable("No participant session is open."))?;
        t.held
            .get_mut(&(wire, record))
            .ok_or_else(|| unavailable("The participant record is unavailable."))?
            .supplied = Some([field].into_iter().collect());
        start.participant = true;
        t.single_root_start = Some(start.clone());
        Ok(start)
    }

    pub(crate) fn extend_attachment_root_field(
        &mut self,
        table: u64,
        record: u64,
        field: u64,
    ) -> Result<(), DataError> {
        let t = self
            .transaction
            .as_mut()
            .ok_or_else(|| unavailable("No record session is open."))?;
        let held = t
            .held
            .get_mut(&(table, record))
            .ok_or_else(|| unavailable("The prepared Root record is unavailable."))?;
        held.supplied
            .as_mut()
            .ok_or_else(|| unavailable("The original input proof is missing."))?
            .insert(field);
        Ok(())
    }

    pub(crate) fn attachment_group_root_shape(&self, retaining_unsubmitted: bool) -> bool {
        self.transaction.as_ref().is_some_and(|t| {
            t.kind == TransactionKind::Record
                && t.ends
                && t.options == TransactionOptions::default()
                && (t.mark.is_none() || retaining_unsubmitted)
                && t.single_root_start.as_ref().is_some_and(|s| !s.participant)
                && self.existing_root_records().is_some()
        })
    }

    pub(crate) fn attachment_part_shape(&self) -> bool {
        if self
            .transaction
            .as_ref()
            .and_then(|t| t.single_root_start.as_ref())
            .is_some_and(|s| !s.rows.members.is_empty() || s.row.is_some())
            && self
                .transaction
                .as_ref()
                .and_then(|t| t.single_root_start.as_ref())
                .is_some_and(|s| s.participant)
        {
            return self.mixed_row_shape(true, false);
        }
        self.transaction.as_ref().is_some_and(|t| {
            t.kind == TransactionKind::Record
                && t.options == TransactionOptions::default()
                && t.single_root_start.as_ref().is_some_and(|s| s.participant)
                && t.mark.is_none()
                && self.owner_transaction.alone()
                && self.existing_root_records().is_some()
        })
    }

    fn scalar_creation_records(&self, root: bool) -> Option<ScalarRecords> {
        let t = self.transaction.as_ref()?;
        if t.kind != TransactionKind::Record
            || t.mark.is_some()
            || (!root && !t.held.is_empty())
            || !self.scalar_session_alone()
        {
            return None;
        }
        let mut rows: Vec<_> = t
            .parked
            .iter()
            .map(|(key, (held, serials))| (*key, held.clone(), serials.clone()))
            .collect();
        for key in &t.order {
            let held = t.held.get(key)?;
            rows.push((
                *key,
                held.clone(),
                t.serials.get(key).cloned().unwrap_or_default(),
            ));
        }
        for (key, held, _) in &rows {
            self.catalog.table(key.0)?;
            if !matches!(held.shown(), Change::New(_)) || held.created.is_none() {
                return None;
            }
        }
        Some(ScalarRecords(rows))
    }

    // Cleanup may fail after the caller accepted a correction. Keep the new
    // records' latest values before ending or parking their live transactions;
    // this snapshot is retention, never permission to recreate them.
    pub(crate) fn asset_draft_records(&self) -> Option<ScalarRecords> {
        if let Some(records) = self
            .mixed_row_records()
            .or_else(|| self.existing_root_records())
        {
            return Some(records);
        }
        let t = self.transaction.as_ref()?;
        if t.kind != TransactionKind::Record {
            return None;
        }
        // An incomplete declared row is not eligible for recovery, but failure
        // retention must still keep every accepted existing and created draft.
        if t.single_root_start
            .as_ref()
            .is_some_and(|s| !s.rows.members.is_empty())
        {
            let mut rows: Vec<_> = t
                .parked
                .iter()
                .map(|(key, (held, serials))| (*key, held.clone(), serials.clone()))
                .collect();
            rows.extend(t.order.iter().filter_map(|key| {
                t.held.get(key).map(|held| {
                    (
                        *key,
                        held.clone(),
                        t.serials.get(key).cloned().unwrap_or_default(),
                    )
                })
            }));
            return Some(ScalarRecords(rows));
        }
        let mut rows: Vec<_> = t
            .parked
            .iter()
            .map(|(key, (held, serials))| (*key, held.clone(), serials.clone()))
            .collect();
        rows.extend(t.order.iter().filter_map(|key| {
            let held = t.held.get(key)?;
            matches!(held.shown(), Change::New(_)).then(|| {
                (
                    *key,
                    held.clone(),
                    t.serials.get(key).cloned().unwrap_or_default(),
                )
            })
        }));
        if rows.iter().any(|(_, held, _)| held.created.is_none()) {
            return None;
        }
        Some(ScalarRecords(rows))
    }

    pub(crate) fn single_root_baseline(&self) -> Option<om_data::SubmissionMark> {
        let t = self.transaction.as_ref()?;
        (t.kind == TransactionKind::Record
            && t.ends
            && t.options == TransactionOptions::default()
            && t.single_root_start.is_none()
            && t.held.is_empty()
            && t.parked.is_empty()
            && t.containers.is_empty())
        .then_some(t.before_record)
        .flatten()
    }

    pub(crate) fn single_root_shape(&self) -> Option<(u64, u64)> {
        if self.transaction.as_ref()?.mark.is_some() || !self.scalar_session_alone() {
            return None;
        }
        self.single_root_live_shape()
    }

    // A save mark and other windows affect reconnect availability, not this
    // live transaction's ownership or its ability to settle its own assets.
    pub(crate) fn single_root_live_shape(&self) -> Option<(u64, u64)> {
        let t = self.transaction.as_ref()?;
        if t.kind != TransactionKind::Record
            || !t.ends
            || t.options != TransactionOptions::default()
            || !t.parked.is_empty()
            || t.held.len() != 1
            || !self.owner_transaction.alone()
        {
            return None;
        }
        let (key, held) = t.held.iter().next()?;
        ((matches!(held.shown(), Change::New(_)) && held.created.is_some()
            || self.existing_root_records().is_some())
            && self.external.route(key.0).is_none())
        .then_some(*key)
    }

    pub(crate) fn restore_scalar_records(
        &mut self,
        records: &ScalarRecords,
        park: bool,
    ) -> Result<Vec<(u64, u64, u64)>, DataError> {
        let mut mapped = Vec::new();
        for ((table, old), held, serials) in &records.0 {
            let mut held = held.clone();
            // Its preview ended with the old owning session. The held
            // creation state supplies its values; do not consume a fresh preview
            // that happens to receive the same numeric id.
            held.preview = None;
            if let Change::Edit { mod_count, .. } = held.change {
                // Revalidate again under the fresh write lock, closing the
                // read-to-lock race. Existing records never reserve a new RID.
                let txn = self.transaction_id()?;
                if self.lock_record(*table, *old, txn)? != Some(mod_count) {
                    return Err(DataError::with_code(
                        306,
                        "The retained record changed before its lock was reacquired.",
                    ));
                }
                self.pinned
                    .insert((self.window, *table, *old), Some(mod_count));
                let t = self
                    .transaction
                    .as_mut()
                    .ok_or_else(|| unavailable("No record session is open."))?;
                t.order.push((*table, *old));
                t.held.insert((*table, *old), held);
            } else {
                let new = self.hold_again(*table, held, serials.clone())?;
                if let Some(start) = self
                    .transaction
                    .as_mut()
                    .and_then(|t| t.single_root_start.as_mut())
                {
                    for row in start.rows.members.values_mut() {
                        if row.key() == Some((*table, *old)) && row.new_row() {
                            *row = scalar_recovery::mixed::RowReceipt::New {
                                table: *table,
                                record: new,
                            };
                        }
                    }
                    if matches!(
                        start.row,
                        Some(scalar_recovery::mixed::RowReceipt::New { .. })
                    ) {
                        start.row = Some(scalar_recovery::mixed::RowReceipt::New {
                            table: *table,
                            record: new,
                        });
                    }
                }
                mapped.push((*table, *old, new));
            }
        }
        if park {
            self.park_new_records();
        }
        Ok(mapped)
    }

    /// [`DataProvider::rollback_record_session`]: the open record session's
    /// held records as [`Self::mark_session`] kept them. What was held
    /// since is dropped from it, not released: the records it locked and
    /// the ids op71 reserved stay locked under it, used, until it ends, as
    /// a held new record Delete Record drops (`hold_delete`). Container
    /// references taken since are released with it, as any its commit
    /// does not store.
    pub(crate) fn rollback_session(&mut self) {
        let Some(mark) = self
            .transaction
            .as_mut()
            .filter(|transaction| transaction.kind == TransactionKind::Record)
            .and_then(|transaction| transaction.mark.take())
        else {
            return;
        };
        // Roll back staged related edits, preserving newly issued ids and
        // creation values for the window's next explicit save (#1174).
        // Already parked participants also enter here after another refusal.
        if mark.owner_untouched && !self.owner_transaction.untouched() {
            self.rollback_owners_parking();
        }
        if let Some(transaction) = &mut self.transaction {
            transaction.held = mark.held;
            transaction.order = mark.order;
            transaction.serials = mark.serials;
        }
        self.remember_scalar_carry();
        self.invalidate_calc_data();
    }

    /// A window record session's commit (`DataProvider::commit_record_session`):
    /// [`Self::commit`], except that a refusal before any record data is submitted puts
    /// the transaction back, its records still held, as FileMaker Pro keeps
    /// a record open whose commit was refused. The serial numbers the
    /// refused attempt reserved stay used.
    pub(crate) fn commit_keeping(&mut self, skip_validation: bool) -> Result<(), DataError> {
        let transaction = self
            .transaction
            .take()
            .ok_or_else(|| unavailable("No transaction is open."))?;
        self.invalidate_calc_data();
        let prepared = match self.prepare_held(&transaction, skip_validation, None) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.transaction = Some(transaction);
                return Err(error);
            }
        };
        match self.submit_commit(PreparedCommit {
            transaction,
            prepared,
        }) {
            CommitOutcome::Committed => Ok(()),
            CommitOutcome::NotSubmitted(error) | CommitOutcome::Ambiguous(error) => Err(error),
        }
    }

    /// Read back the records of `commit`, sent with an unknown outcome
    /// ([`Readback`]). Only what was read is compared: nothing is computed
    /// again.
    pub(crate) fn read_back(&mut self, commit: &PreparedCommit) -> Readback {
        let (mut applied, mut before) = (0, 0);
        for p in &commit.prepared {
            let current = match self.read_record(p.table.id, p.record) {
                Ok(current) => current,
                Err(error) => return Readback::Unresolved(error.message),
            };
            let change = commit
                .transaction
                .held
                .get(&(p.table.id, p.record))
                .map(|held| &held.change);
            let now = current
                .as_ref()
                .and_then(|node| super::records::mod_count(&stored_record(p.record, node)).ok());
            // The version an existing record was locked at: as before the
            // commit while the host still holds it there.
            let locked = match change {
                Some(Change::Edit { mod_count, .. }) => Some(*mod_count),
                Some(Change::Imported {
                    mod_count: Some(mod_count),
                    ..
                }) => Some(*mod_count),
                _ => None,
            };
            let unchanged = locked.is_some() && now == locked;
            let state = match (&p.expected, &p.cells, &current) {
                // A deletion: applied once the record is gone.
                (None, None, None) => Some(true),
                (None, None, Some(_)) => Some(false),
                // Gone: not applied only for a record the commit created.
                (_, _, None) => matches!(
                    change,
                    Some(
                        Change::New(_)
                            | Change::Imported {
                                mod_count: None,
                                ..
                            }
                    )
                )
                .then_some(false),
                (Some((values, mod_count)), _, Some(_)) => {
                    if self
                        .verify_record(&p.table, p.record, values, *mod_count)
                        .is_ok()
                    {
                        Some(true)
                    } else {
                        unchanged.then_some(false)
                    }
                }
                (None, Some((cells, mod_count)), Some(_)) => {
                    if self
                        .verify_cells(&p.table, p.record, cells, *mod_count)
                        .is_ok()
                    {
                        Some(true)
                    } else {
                        unchanged.then_some(false)
                    }
                }
            };
            match state {
                Some(true) => applied += 1,
                Some(false) => before += 1,
                None => {
                    return Readback::Unresolved(format!(
                        "Record {} holds neither the saved values nor the ones before.",
                        p.record
                    ))
                }
            }
        }
        let count = commit.prepared.len();
        if applied == count {
            Readback::Applied
        } else if before == count && !commit.transaction.ends {
            Readback::NotApplied
        } else if before == count {
            Readback::Unresolved("Its records are no longer locked for the save.".into())
        } else {
            Readback::Unresolved("Some of its records were saved and others not.".into())
        }
    }

    /// Settle a kept copy of a commit read back as applied ([`Readback`]),
    /// as a successful submission settles it: the stored container members
    /// its records replaced or deleted are released, and so are references
    /// it took that no record stored. Once, for a commit whose sending kept
    /// them (an unknown outcome keeps them, `end_taken`).
    pub(crate) fn settle_applied(&mut self, commit: PreparedCommit) {
        let PreparedCommit {
            transaction,
            prepared,
        } = commit;
        let mut used = BTreeMap::new();
        for p in &prepared {
            for member in &p.acquired {
                *used.entry((p.table.id, member.clone())).or_insert(0) += 1;
            }
        }
        for p in prepared {
            self.release_members(p.table.id, p.released);
        }
        self.release_transaction_containers(&transaction, &used);
    }

    /// [`Self::prepare_commit`], except that a refusal puts the transaction
    /// back, its records still held, rather than end it: a window's record
    /// session whose commit is refused before any record data is submitted
    /// stays open (#1095), in every file of the commit (#1006). The serial
    /// numbers the refused attempt reserved stay used.
    pub(crate) fn prepare_keeping(
        &mut self,
        skip_validation: bool,
        others: Option<&HeldRecords>,
    ) -> Result<PreparedCommit, DataError> {
        let transaction = self
            .transaction
            .take()
            .ok_or_else(|| unavailable("No transaction is open."))?;
        self.invalidate_calc_data();
        match self
            .prepare_held(&transaction, skip_validation, others)
            .and_then(check_wire)
        {
            Ok(prepared) => Ok(PreparedCommit {
                transaction,
                prepared,
            }),
            Err(error) => {
                self.transaction = Some(transaction);
                Err(error)
            }
        }
    }

    /// Put back the transaction of a prepared commit that is not submitted
    /// after all, its records still held ([`Self::prepare_keeping`]).
    pub(crate) fn restore_prepared(&mut self, commit: PreparedCommit) {
        self.transaction = Some(commit.transaction);
    }

    /// Undo [`Self::hold_transaction_end`]: the open transaction ends itself
    /// again (the commit that held its end kept it open instead).
    pub(crate) fn release_transaction_end(&mut self) {
        if let Some(transaction) = self.transaction.as_mut() {
            transaction.ends = true;
        }
    }

    /// Commit Transaction: [`Self::prepare_commit`], then
    /// [`Self::submit_commit`], and what they did. The transaction's part
    /// ends either way, its locks released unless it shares them
    /// (`Transaction::ends`). A failure before submission writes nothing;
    /// after submission its outcome can be unknown.
    pub(crate) fn commit(&mut self, skip_validation: bool) -> CommitOutcome {
        match self.prepare_commit(skip_validation, None) {
            Ok(commit) => self.submit_commit(commit),
            Err(error) => CommitOutcome::NotSubmitted(error),
        }
    }

    /// Commit's first stage: take the open transaction and apply the record
    /// rules to every held record, building each table's uploads. It sends
    /// no op14 or op70, but it reserves the serial numbers not taken yet
    /// (op72), which stay used, and reads indexes, related records and
    /// container data (module docs). A refusal ends the transaction as
    /// Revert does and is the error.
    ///
    /// `others`: the records the save's other participants hold, as their
    /// prepared commits write them, by this file's ids for their tables
    /// (#1167). This file's rules and related-values lists read them over
    /// the stored ones, rather than what the save is changing.
    pub(crate) fn prepare_commit(
        &mut self,
        skip_validation: bool,
        others: Option<&HeldRecords>,
    ) -> Result<PreparedCommit, DataError> {
        let transaction = self
            .transaction
            .take()
            .ok_or_else(|| unavailable("No transaction is open."))?;
        self.invalidate_calc_data();
        match self
            .prepare_held(&transaction, skip_validation, others)
            .and_then(check_wire)
        {
            Ok(prepared) => Ok(PreparedCommit {
                transaction,
                prepared,
            }),
            Err(error) => {
                self.end_taken(&transaction, false);
                Err(error)
            }
        }
    }

    /// Commit's second stage: upload each table's prepared records (op14),
    /// commit them all in one op70 and read every record back. The
    /// transaction ends either way (op65).
    pub(crate) fn submit_commit(&mut self, commit: PreparedCommit) -> CommitOutcome {
        let PreparedCommit {
            transaction,
            prepared,
        } = commit;
        let mut submitted = false;
        let result = self.submit_prepared(&transaction, prepared, &mut submitted);
        self.end_taken(&transaction, submitted);
        match result {
            Ok(()) => CommitOutcome::Committed,
            Err(error) if submitted => CommitOutcome::Ambiguous(error),
            Err(error) => CommitOutcome::NotSubmitted(error),
        }
    }

    /// [`Self::submit_commit`] for a window's record commit across files
    /// (#1006), whose caller keeps a copy of `commit` made before: a
    /// failure before op70 leaves its part as it was, sending no op65 and
    /// releasing no container reference, for that copy to go back or go
    /// again. Any other outcome ends it as `submit_commit` does.
    pub(crate) fn submit_keeping(&mut self, commit: PreparedCommit) -> CommitOutcome {
        let PreparedCommit {
            transaction,
            prepared,
        } = commit;
        let mut submitted = false;
        let result = self.submit_prepared(&transaction, prepared, &mut submitted);
        match result {
            Ok(()) => {
                self.end_taken(&transaction, true);
                CommitOutcome::Committed
            }
            Err(error) if submitted => {
                self.end_taken(&transaction, true);
                CommitOutcome::Ambiguous(error)
            }
            Err(error) => CommitOutcome::NotSubmitted(error),
        }
    }

    /// Drop a prepared commit as Revert drops a transaction: op65 releases
    /// its locks (`Transaction::ends`) and its container references are
    /// released. The serial numbers it reserved stay used.
    pub(crate) fn abandon_commit(&mut self, commit: PreparedCommit) {
        self.end_taken(&commit.transaction, false);
    }

    fn prepare_held(
        &mut self,
        transaction: &Transaction,
        skip_validation: bool,
        others: Option<&HeldRecords>,
    ) -> Result<Vec<Prepared>, DataError> {
        let entry = Entry {
            script: false,
            skip_data_entry_validation: transaction.options.skip_validation || skip_validation,
            skip_auto_enter: transaction.options.skip_auto_enter,
            keep_written: false,
        };
        let environment = self.record_environment()?;
        let txn = transaction.txn;
        // Unique values among the held records, by (table, field).
        let mut unique = UniqueHeld::new();
        let mut prepared = Vec::new();
        // What each held record's formulas read of related tables, and those
        // tables' held records, read once for the whole commit (#187); each
        // is then replaced by the record as the commit writes it, its
        // computed values and serials included, once prepared.
        let mut reads_of: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
        // Whether a related-values list may read the records computed so far
        // (`computed`, below).
        let mut lists = false;
        for key @ (table, _) in &transaction.order {
            // A deletion evaluates no rules: its table needs no plan.
            let deleted = transaction
                .held
                .get(key)
                .is_some_and(|held| matches!(held.change, Change::Deleted));
            if !deleted && !reads_of.contains_key(table) {
                let (_, plan) = self.table_plan(*table)?;
                reads_of.insert(*table, plan.related_tables());
                lists |= plan.checks_related_lists();
            }
        }
        let read_tables: BTreeSet<u64> = reads_of.values().flatten().copied().collect();
        // Over the other participants' records, final once prepared: their
        // tables are related files', never this file's own (#1167).
        let mut overlay =
            self.held_records_over(others, Self::held_of(transaction, &read_tables))?;
        // Literal held cells are already visible. Only values the rules
        // may still compute are unknown until their immutable row is ready.
        let mut inputs = BTreeMap::new();
        let mut unknown = crate::preview::Unknown::new();
        for (key, held) in &transaction.held {
            let (mode, existing, values, serials) = match &held.change {
                Change::Deleted => continue,
                Change::New(values) => {
                    let (values, serials) = held_serials(Some(transaction), *key, values);
                    (WriteMode::Create, BTreeMap::new(), values, serials)
                }
                Change::Edit { values, .. } => {
                    let node = self.read_record(key.0, key.1)?.ok_or_else(|| {
                        DataError::with_code(RECORD_MISSING, "A changed record no longer exists.")
                    })?;
                    (
                        WriteMode::Update,
                        text_values(&stored_record(key.1, &node)),
                        values.clone(),
                        BTreeMap::new(),
                    )
                }
                Change::Imported {
                    mod_count,
                    cells,
                    later,
                } => {
                    let mut node = match mod_count {
                        None => OwnedNode::list(serial::encode(key.1), Vec::new()),
                        Some(_) => self.read_record(key.0, key.1)?.ok_or_else(|| {
                            DataError::with_code(
                                RECORD_MISSING,
                                "An imported record no longer exists.",
                            )
                        })?,
                    };
                    apply_values(&mut node, cells.clone());
                    (
                        WriteMode::Update,
                        text_values(&stored_record(key.1, &node)),
                        later.clone(),
                        BTreeMap::new(),
                    )
                }
            };
            let (_, plan) = self.table_plan(key.0)?;
            let entry = Entry {
                script: held.script,
                keep_written: held.script && mode == WriteMode::Create,
                ..entry
            };
            let fields = plan.unprepared_fields(mode, &existing, &values, entry, &serials)?;
            unknown.entry(key.0).or_default().insert(key.1, fields);
            inputs.insert(*key, (mode, existing, values, serials));
        }
        let mut done: BTreeSet<(u64, u64)> = transaction
            .order
            .iter()
            .filter(|key| !transaction.held.contains_key(key))
            .copied()
            .collect();
        // The last prepared record's row, put into the overlay when the next
        // record begins: its environment, which shares the overlay, is gone
        // then, so the overlay changes in place instead of being copied.
        let mut pending: Option<(u64, u64, BTreeMap<u64, String>)> = None;
        // Every record computed so far, as the commit writes it, the other
        // participants' among them: what a related-values list reads of
        // them (related_lists.rs), kept when a table checks one.
        let mut computed = others.cloned().unwrap_or_default();
        // `None`: every table's records; else those of no table.
        let no_tables = BTreeSet::new();
        let kept = if lists { None } else { Some(&no_tables) };
        // At most one final preparation per record. Discovery may revisit
        // candidates after a dependency becomes final; without progress the
        // remaining graph is cyclic or unresolved, never a fixed point.
        while done.len() < transaction.order.len() {
            if let Some((table, record, row)) = pending.take() {
                std::sync::Arc::make_mut(overlay.get_or_insert_with(Default::default))
                    .hold(table, record, row);
            }
            let mut next = None;
            for key in transaction.order.iter().filter(|key| !done.contains(key)) {
                let held = &transaction.held[key];
                let Some((mode, existing, values, serials)) = inputs.get(key) else {
                    next = Some(*key); // a deletion evaluates no rules
                    break;
                };
                let (_, plan) = self.table_plan(key.0)?;
                let mut discovery = RecordEnvironment {
                    globals: self.record_globals(key.0, &plan)?,
                    context: held.context.clone(),
                    held: overlay.clone(),
                    serials: serials.clone(),
                    ..environment.clone()
                };
                if let Some(created) = &held.created {
                    created.stamp.apply(&mut discovery);
                }
                let entry = Entry {
                    script: held.script,
                    keep_written: held.script && *mode == WriteMode::Create,
                    ..entry
                };
                if self.held_reads_ready(
                    &plan,
                    (*mode, entry, Written::held(*mode, key.1)),
                    (existing, values),
                    &discovery,
                    &unknown,
                )? {
                    next = Some(*key);
                    break;
                }
            }
            let key = next.as_ref().ok_or_else(|| DataError::with_code(error::UNAVAILABLE,
                "Held records have cyclic or unresolved calculation dependencies. No values were changed."))?;
            let held = &transaction.held[key];
            let (table, record) = *key;
            let reads = reads_of.get(&table).cloned().unwrap_or_default();
            if let Some(records) = unknown.get_mut(&table) {
                records.remove(&record);
            }
            done.insert(*key);
            let record_key = serial::encode(record);
            if matches!(held.change, Change::Deleted) {
                // Prepared from the table's fields and the stored record's
                // container references alone, with no field-entry plan, as
                // the ordinary deletion is: a table whose rules OpenMaker
                // cannot plan still deletes.
                let table_def = self.table_fields(table)?;
                let released = match self.read_record(table, record)? {
                    Some(node) => super::containers::references(
                        &table_def,
                        &text_values(&stored_record(record, &node)),
                        None,
                    ),
                    None => Vec::new(),
                };
                prepared.push(Prepared {
                    table: table_def,
                    record,
                    node: None,
                    entry: OwnedNode::leaf(record_key, vec![write::COMMIT_DELETED]),
                    expected: None,
                    cells: None,
                    released,
                    acquired: Vec::new(),
                });
                computed.delete(table, record);
                continue;
            }
            let (table_def, plan) = self.table_plan(table)?;
            // The session's global values its formulas read (5.17), for
            // an imported row's later edits as for any held record.
            let mut environment = RecordEnvironment {
                globals: self.record_globals(table, &plan)?,
                context: held.context.clone(),
                held: overlay.clone().filter(|_| !reads.is_empty()),
                ..environment.clone()
            };
            if let Some(created) = &held.created {
                created.stamp.apply(&mut environment);
            }
            let entry = Entry {
                script: held.script,
                keep_written: held.script && matches!(held.change, Change::New(_)),
                ..entry
            };
            if let Change::Imported {
                mod_count,
                cells,
                later,
            } = &held.change
            {
                // Its related-values lists were checked at its import, before
                // what else the transaction holds (a user's entry, the widest
                // that checks lists).
                let holding: BTreeSet<(u64, u64)> = transaction.held.keys().copied().collect();
                self.refuse_held_related_lists(
                    &table_def,
                    &plan,
                    Entry::default(),
                    (cells, record),
                    crate::related_lists::Holding::uncomputed(&holding),
                )?;
                let ready = self.commit_imported(
                    &table_def,
                    &plan,
                    record,
                    (*mod_count, cells, later, &held.typed),
                    (entry, &environment, txn),
                    &mut unique,
                )?;
                prepared.push(ready);
                pending = Self::prepared_row(Some(&read_tables), prepared.last());
                if let Some((table, record, row)) = Self::prepared_row(kept, prepared.last()) {
                    computed.hold(table, record, row);
                }
                continue;
            }
            let (values, mode, mut node, base) = match &held.change {
                Change::Imported { .. } => unreachable!("written above"),
                Change::Deleted => unreachable!("prepared above"),
                Change::New(values) => (
                    values,
                    WriteMode::Create,
                    OwnedNode::list(record_key.clone(), Vec::new()),
                    None,
                ),
                Change::Edit { mod_count, values } => {
                    let node = self.read_record(table, record)?.ok_or_else(|| {
                        DataError::with_code(RECORD_MISSING, "A changed record no longer exists.")
                    })?;
                    if super::records::mod_count(&stored_record(record, &node))? != *mod_count {
                        return Err(DataError::with_code(
                            STALE_RECORD,
                            "A changed record was modified by another client.",
                        ));
                    }
                    (values, WriteMode::Update, node, Some(*mod_count))
                }
            };
            let existing = match base {
                Some(_) => text_values(&stored_record(record, &node)),
                None => BTreeMap::new(),
            };
            // A new record's serials taken at New Record are entered again
            // by its rules, not as values it was given.
            let (values, serials) = match &held.change {
                Change::New(_) => held_serials(Some(transaction), *key, values),
                _ => (values.clone(), BTreeMap::new()),
            };
            let environment = RecordEnvironment {
                serials,
                ..environment.clone()
            };
            let ready = self.prepare_record(
                &plan,
                (mode, entry, Written::held(mode, record)),
                &existing,
                &values,
                &environment,
            )?;
            self.refuse_related_lists(&table_def, &plan, entry, entered(&ready.values))?;
            let unique_fields = plan.unique_fields_for(entry);
            let file_language = self.file_language()?;
            note_unique(
                &mut unique,
                &table_def,
                unique_fields.clone(),
                &ready.values,
                (record, file_language),
            )?;
            self.check_unique(&table_def, unique_fields, &ready.values, record, txn)?;
            self.check_lookups(&table_def, &plan, entry, &ready.values, record)?;
            let holding: BTreeSet<(u64, u64)> = transaction.held.keys().copied().collect();
            self.check_related_lists(
                &table_def,
                &plan,
                (entry, &held.typed),
                (&existing, &ready.values),
                (
                    Written::held(mode, record),
                    crate::related_lists::Holding {
                        held: &holding,
                        done: &done,
                        computed: Some(&computed),
                    },
                ),
            )?;
            // A script's repetitions are read back cell by cell.
            let expected = match held.script {
                true => None,
                false => Some(expected_values(&table_def, &ready.values)?),
            };
            let written = ready.values.clone();
            let (entry_node, next) = match base {
                None => (OwnedNode::leaf(record_key, vec![write::COMMIT_NEW]), 0),
                Some(mod_count) => {
                    if ready.changed_fields.is_empty() {
                        // Even an unchanged dependency has a final row.
                        if read_tables.contains(&table) {
                            pending = Some((table, record, existing.clone()));
                        }
                        if lists {
                            computed.hold(table, record, existing.clone());
                        }
                        continue;
                    }
                    let next = mod_count.checked_add(1).ok_or_else(|| {
                        DataError::new("A record's modification count is exhausted.")
                    })?;
                    node.set_child(OwnedNode::leaf(key_bytes(MOD_COUNT), mod_count_value(next)));
                    let changed = ready
                        .changed_fields
                        .iter()
                        .map(|field| OwnedNode::leaf(key_bytes(*field), Vec::new()))
                        .collect();
                    (OwnedNode::list(record_key, changed), next)
                }
            };
            let released = match base {
                Some(_) => super::containers::references(
                    &table_def,
                    &existing,
                    Some(&ready.changed_fields),
                ),
                None => Vec::new(),
            };
            apply_values(&mut node, ready.values);
            let acquired = super::containers::references(
                &table_def,
                &text_values(&stored_record(record, &node)),
                match base {
                    Some(_) => Some(ready.changed_fields.as_slice()),
                    None => None,
                },
            );
            prepared.push(Prepared {
                table: table_def,
                record,
                node: Some(node),
                entry: entry_node,
                expected: expected.map(|expected| (expected, next)),
                cells: held.script.then_some((written, next)),
                released,
                acquired,
            });
            pending = Self::prepared_row(Some(&read_tables), prepared.last());
            if let Some((table, record, row)) = Self::prepared_row(kept, prepared.last()) {
                computed.hold(table, record, row);
            }
        }
        Ok(prepared)
    }

    fn submit_prepared(
        &mut self,
        transaction: &Transaction,
        mut prepared: Vec<Prepared>,
        submitted: &mut bool,
    ) -> Result<(), DataError> {
        if prepared.is_empty() {
            return Ok(());
        }
        let txn = transaction.txn;
        // Upload each table's records, then commit them all in one op70.
        prepared.sort_by_key(|p| (p.table.id, p.record));
        let tables = wire_tables(&prepared);
        let session = self.session_mut()?;
        for (table, nodes, _) in &tables {
            if !nodes.is_empty() {
                session
                    .upload_records(&key_bytes(*table), nodes.clone())
                    .map_err(|e| failure("upload the transaction's records", e))?;
            }
        }
        *submitted = true;
        session
            .commit_tables(
                tables
                    .into_iter()
                    .map(|(table, _, entries)| (key_bytes(table), entries))
                    .collect(),
                txn,
            )
            .map_err(|e| failure("commit the transaction", e))?;
        // The commit reply carries no status (write-path.md 3.4).
        let mut used = BTreeMap::new();
        for p in &prepared {
            for member in &p.acquired {
                *used.entry((p.table.id, member.clone())).or_insert(0) += 1;
            }
        }
        for p in &prepared {
            if let Some((cells, mod_count)) = &p.cells {
                self.verify_cells(&p.table, p.record, cells, *mod_count)?;
                continue;
            }
            match &p.expected {
                Some((expected, mod_count)) => {
                    self.verify_record(&p.table, p.record, expected, *mod_count)?;
                }
                None => {
                    if self.read_record(p.table.id, p.record)?.is_some() {
                        return Err(DataError::new(
                            "The server did not delete a record of the transaction.",
                        ));
                    }
                    self.wrote_version((p.table.id, p.record), None);
                }
            }
        }
        // A complete statusless reply does not establish that any write
        // was kept. Retain old stored members until every readback passes.
        for p in prepared {
            self.release_members(p.table.id, p.released);
        }
        self.release_transaction_containers(transaction, &used);
        Ok(())
    }
}

/// Unique values among a transaction's held records: by (table, field),
/// each text index key and the record holding it.
type UniqueHeld = BTreeMap<(u64, u64), BTreeMap<Vec<u8>, u64>>;

/// Note the values `values` gives `fields` of `record` among the held
/// records' unique values; another held record with the same value is 504.
fn note_unique(
    unique: &mut UniqueHeld,
    table: &Table,
    fields: Vec<u64>,
    values: &EncodedChanges,
    (record, file_language): (u64, Option<u32>),
) -> Result<(), DataError> {
    for field in fields {
        if let Some(Some(value)) = values.get(&field) {
            let text = om_scsu::decode_scsu(value)
                .map_err(|e| failure("decode a unique value", format!("{e:?}")))?;
            let key = super::records::checked_keys(table, field, file_language)?
                .key(&text)
                .unwrap_or_default();
            let seen = unique.entry((table.id, field)).or_default();
            if let Some(other) = seen.insert(key, record) {
                if other != record {
                    let name = table.field(field).map_or("", |f| f.name.as_str());
                    return Err(DataError::with_code(
                        error::NOT_UNIQUE,
                        format!("Field {name} requires a unique value."),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A record as the transaction holds it after an import: `base` (the
/// stored record, if any) with the imported `cells` of first repetitions.
fn held_cells(table: &Table, id: i64, base: Option<Record>, cells: &EncodedChanges) -> Record {
    let values: Cells = cells
        .iter()
        .map(|(cell, value)| {
            let text = value
                .as_ref()
                .map(|v| om_scsu::decode_scsu(v).unwrap_or_default());
            (*cell, text)
        })
        .collect();
    held_record(table, id, base, &values)
}

/// A record as the transaction holds it: `base` (the stored record, if
/// any) with `values` set; a page shows first repetitions only.
fn held_record(table: &Table, id: i64, base: Option<Record>, values: &Cells) -> Record {
    let mut record = base.unwrap_or_else(|| Record {
        id,
        values: vec![None; table.fields.len()],
    });
    for (cell, value) in values {
        if let Some(i) = table.fields.iter().position(|f| f.id == *cell) {
            if let Some(slot) = record.values.get_mut(i) {
                *slot = value.clone();
            }
        }
    }
    record
}

#[cfg(test)]
#[path = "../../om-server/tests/fixtures/tasks.rs"]
pub(super) mod fixture;

#[cfg(test)]
mod tests {
    use super::fixture::{TempFile, ACCOUNT, FILE, LAYOUT, PASSWORD};
    use super::*;
    use om_data::{Connection, DataProvider};
    use om_protocol::MachineInfo;
    use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

    /// The Tasks file on a loopback host, and its certificate.
    fn host(tag: &str) -> (TempFile, u16, Vec<u8>) {
        let file = TempFile::new(tag, &[], true);
        let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca_pem = generated.cert.pem().into_bytes();
        let identity =
            Identity::from_pem(&ca_pem, generated.key_pair.serialize_pem().as_bytes()).unwrap();
        let config = ServerConfig {
            identity: ServerIdentity {
                name: format!("transaction-{tag}"),
                version_string: "OpenMaker test".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            files: vec![HostedFile {
                name: FILE.into(),
                path: file.0.clone(),
            }],
            insecure_files: false,
            record_dir: None,
        };
        let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || server.serve());
        (file, port, ca_pem)
    }

    fn client(ca_pem: &[u8]) -> NativeProvider {
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        NativeProvider::new(NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.key_pair.serialize_pem().into_bytes(),
            additional_ca_pem: ca_pem.to_vec(),
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        })
    }

    /// A provider with the Tasks file open on a connection of its own.
    fn open(port: u16, ca_pem: &[u8]) -> NativeProvider {
        let mut provider = client(ca_pem);
        provider
            .open(&Connection {
                host: "127.0.0.1".into(),
                port,
                file: FILE.into(),
                account: ACCOUNT.into(),
                password: PASSWORD.into(),
            })
            .unwrap();
        provider
    }

    fn name(value: &str) -> om_data::FieldChanges {
        [("Name".to_owned(), Some(value.to_owned()))].into()
    }

    /// #1006's seam. A transaction keeps the kind it was begun as, and a
    /// provider joins it under its id only on its own connection: a handle
    /// of that connection does, a second connection to the same host is
    /// refused (3). A prepared commit keeps the transaction's locks and
    /// stores nothing; abandoning it releases them as Revert does.
    #[test]
    fn a_transaction_is_joined_only_on_its_own_connection() {
        let (_file, port, ca_pem) = host("joined");
        let layout = LAYOUT as u32;
        let mut root = open(port, &ca_pem);
        let mut other = open(port, &ca_pem);
        root.records(layout, 0, 10).unwrap();
        other.records(layout, 0, 10).unwrap();

        root.begin_kind(TransactionOptions::default(), TransactionKind::Record)
            .unwrap();
        let shared = root.shared_transaction().unwrap();
        let refused = other.begin_under(&shared).unwrap_err();
        assert_eq!(refused.code, Some(3), "{refused}");
        assert_eq!(other.transaction_kind(), None);
        let unopened = client(&ca_pem).begin_under(&shared).unwrap_err();
        assert_eq!(unopened.code, Some(3), "{unopened}");
        let mut joined = client(&ca_pem);
        joined.session = Some(root.session.as_ref().unwrap().share());
        joined.begin_under(&shared).unwrap();
        assert_eq!(joined.transaction_kind(), Some(TransactionKind::Record));
        assert_eq!(joined.transaction_id().unwrap(), shared.txn);
        joined.revert().unwrap();

        root.update_record(layout, 1, name("Held")).unwrap();
        let commit = root.prepare_commit(false, None).unwrap();
        assert!(!root.in_transaction());
        let locked = other.update_record(layout, 1, name("Other")).unwrap_err();
        assert_eq!(locked.code, Some(301), "{locked}");
        root.abandon_commit(commit);
        let page = open(port, &ca_pem).records(layout, 0, 10).unwrap();
        let column = page.fields.iter().position(|f| f.name == "Name").unwrap();
        let stored = page.records.iter().find(|r| r.id == 1).unwrap();
        assert_eq!(stored.values[column].as_deref(), Some("Write tests"));
        other.update_record(layout, 1, name("Other")).unwrap();
    }
}
