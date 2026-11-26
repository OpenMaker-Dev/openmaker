//! A new record's values before it is written (#1047,
//! [`DataProvider::preview_new_record`]): what a window shows of a portal
//! row the user began, and what its triggers read, before the record
//! commit writes it.
//!
//! A preview computes the record's rules as its write would, with
//! validation left to the write: auto-entered values, serial numbers
//! generated on creation and stored calculations, over the cells the write
//! gives ([`om_data::rules::RecordPlan::preview_fields`]). Before the commit
//! opens its transaction, the window's unsaved records (its own record's
//! typed cells, a parent not created yet, the other rows begun in its
//! portals) are the window's, and a read would get the host's stored record
//! instead. So a preview reads no other record, and leaves out a lookup, a
//! related field, an aggregate of related records and every field computed
//! from one, unless the window gives them
//! ([`DataProvider::preview_new_record_over`], #1063): then it reads
//! through them, as a held-record overlay (`NativeProvider::pending_overlay`), over
//! this file's tables, and over the opened file's for a routed row
//! (`CallerReads::pending`), each computed as its commit would compute it. A
//! value of them its commit may not store (left out, drawn again) is
//! unknown, record by record, and so is every value read from or computed
//! over one: such a field of the written row is left out, the rest is
//! read. They are not
//! used when one is a new record of another table than the written row's
//! (a parent the window has not created, whose keys the commit may
//! compute), a record of a table the reading file cannot overlay, or a
//! typed value not of its type. Always
//! left out, with every field computed from one: a `Random` draw (the
//! commit would draw again), and a formula it cannot evaluate yet (a record
//! number the window did not give, #1038, a self-join's own membership,
//! #1035). The write computes each over the records as the commit holds
//! them, or refuses it as it would without a preview. Nothing is written:
//! no op71, op14, op70 or op103. The serial numbers generated on creation
//! are reserved (op72) once, now, and stay used whatever follows, a gap
//! like a reverted record's; those generated on commit are reserved by the
//! write.
//!
//! The provider keeps what the preview drew under its id ([`Snapshot`]):
//! those serial numbers, the UUID, date, time and timestamp, and what the
//! operation supplied ([`WriteContext`]; the record number is the creating
//! write's own, `None` included, #1084). The
//! write naming the id ([`om_data::ScriptWrite::prepared`]) computes with
//! them, so a value from inputs no trigger changed is the value the preview
//! showed, and no serial number is reserved twice; a changed input is
//! computed again. A preview call naming the id computes it again over the
//! cells it gives then (a trigger changed one, a vetoed commit is retried)
//! with the same snapshot, under the same id. A write outside a transaction
//! uses the snapshot up as it sends op71. One held in a transaction keeps
//! it with the held record ([`super::transaction`]) and uses it up when
//! Commit Transaction submits; a reverted or refused commit leaves it for
//! the next attempt. A preview ends with [`DataProvider::discard_prepared`],
//! or when the file closes or reconnects. A write naming a preview the
//! provider does not hold, holds for another table or layout, or already
//! holds in the open transaction is refused (3) before anything is sent,
//! rather than stored with values other than those shown.
//!
//! A related record of another file's table, and a new record of a layout
//! of another file's table (#43), are previewed by that file's provider,
//! through the table's route as `write_record` writes them
//! (related_owner.rs): the owner keeps the snapshot under an id of its own,
//! and this file maps its id to it.

use super::*;
use om_data::rules::HeldRecords;
use om_data::rules::{Entry, RecordEnvironment, WriteContext, WriteMode};
use om_data::{NewRecordPreview, PendingRecord, PreparedId, ScriptWrite};

use super::records::{CurrentSet, Origin};

/// What a provider's previews hold (module docs).
#[derive(Default)]
pub(crate) struct Previews {
    held: BTreeMap<PreparedId, Preview>,
    /// The last id handed out: never reused, also after a reconnect, so an
    /// id a window kept from before cannot name a later preview.
    last: u64,
}

impl Previews {
    /// Forget every preview, keeping the ids handed out used (a closed or
    /// reconnected file).
    pub(crate) fn clear(&mut self) {
        self.held.clear();
    }

    /// This file's own previews, taken out to outlive a reconnect (#152):
    /// the serial numbers and the clock and UUID they drew are values the
    /// lost session issued, which their records' saves reuse. A routed
    /// one goes: its owner's preview went with that file's session.
    pub(crate) fn take_own(&mut self) -> KeptPreviews {
        KeptPreviews(
            std::mem::take(&mut self.held)
                .into_iter()
                .filter(|(_, preview)| matches!(preview, Preview::Own(_)))
                .collect(),
        )
    }

    /// Put back what [`Self::take_own`] took.
    pub(crate) fn put_back(&mut self, kept: KeptPreviews) {
        self.held.extend(kept.0);
    }

    /// Whether preview `id` outlives a reconnect ([`Self::take_own`]).
    pub(crate) fn survives_reconnect(&self, id: PreparedId) -> bool {
        matches!(self.held.get(&id), Some(Preview::Own(_)))
    }

    fn insert(&mut self, preview: Preview) -> PreparedId {
        self.last += 1;
        let id = PreparedId(self.last);
        self.held.insert(id, preview);
        id
    }
}

/// Previews kept across a reconnect ([`Previews::take_own`]).
pub(crate) struct KeptPreviews(BTreeMap<PreparedId, Preview>);

enum Preview {
    /// A record of this file's table `snapshot.table`.
    Own(Snapshot),
    /// A related record of imported table `table`, or a new record of a
    /// layout of it, written through this file's `layout` and previewed by
    /// the owner `route` reaches as its preview `owner`.
    Routed {
        route: external::TableRoute,
        table: u64,
        layout: u32,
        owner: PreparedId,
    },
}

/// What a preview drew that its write reuses (module docs).
struct Snapshot {
    /// The base table, this provider's id.
    table: u64,
    /// The layout of this file the write goes through: `None` for a write
    /// routed here from another file's window.
    layout: Option<u32>,
    /// Serial numbers generated on creation, reserved by the preview.
    serials: BTreeMap<u64, String>,
    stamp: Stamp,
    context: WriteContext,
}

/// The clock and UUID a record's rules read ([`RecordEnvironment`]),
/// drawn once by a preview and kept by its held record.
#[derive(Clone, Debug)]
pub(crate) struct Stamp {
    uuid: String,
    date: String,
    time: String,
    timestamp: String,
}

impl Stamp {
    pub(crate) fn of(environment: &RecordEnvironment) -> Self {
        Self {
            uuid: environment.uuid.clone(),
            date: environment.date.clone(),
            time: environment.time.clone(),
            timestamp: environment.timestamp.clone(),
        }
    }

    pub(crate) fn apply(&self, environment: &mut RecordEnvironment) {
        environment.uuid.clone_from(&self.uuid);
        environment.date.clone_from(&self.date);
        environment.time.clone_from(&self.time);
        environment.timestamp.clone_from(&self.timestamp);
    }
}

/// What a new record held in a transaction ([`super::transaction`]) is
/// given before its commit: the clock and UUID drawn when New Record
/// created it (its preview's, when written from one), which its formulas
/// read until the commit and the commit computes it with, and its values
/// computed with them over its held cells ([`NativeProvider::created_values`]).
#[derive(Clone, Debug)]
pub(crate) struct Created {
    pub(crate) stamp: Stamp,
    pub(crate) values: BTreeMap<u64, Option<String>>,
}

/// What a write reuses of its preview: the serial numbers taken when the
/// record is created, and the clock and UUID.
pub(crate) struct Reuse {
    pub(crate) serials: BTreeMap<u64, String>,
    pub(crate) stamp: Stamp,
}

/// What the window making a preview supplies for the formulas of its
/// unsaved records ([`NativeProvider::pending_overlay`]): its layout of this
/// file, the current privilege set, and the number of the record computed
/// (its own record's, for [`DataProvider::compute_pending`]).
#[derive(Clone)]
struct Operation {
    layout: Option<u32>,
    set: CurrentSet,
    record_number: Option<u64>,
}

/// The fields of the window's unsaved records whose values may not be what
/// their commits store, by table id and record id (#1063): the ids they are
/// read under in the overlay.
pub(crate) type Unknown = BTreeMap<u64, BTreeMap<u64, BTreeSet<u64>>>;

/// What a preview reads of the window's unsaved records: their overlay
/// (`None`: nothing to overlay), and which of their values are unknown.
type Pending = (Option<std::sync::Arc<HeldRecords>>, Unknown);

/// Whether `unknown` marks field `field` of table `table` on record
/// `record` (`None`: on any of its records).
pub(crate) fn marked(unknown: &Unknown, (table, record, field): (u64, Option<u64>, u64)) -> bool {
    unknown.get(&table).is_some_and(|records| match record {
        Some(record) => records.get(&record).is_some_and(|f| f.contains(&field)),
        None => records.values().any(|f| f.contains(&field)),
    })
}

/// Whether field `field` of table `table`, on record `record` (`None`: on
/// any of its records), reads as a value its commit may not store:
/// one `unknown` marks, or one computed when read from one
/// ([`om_data::rules::unknown_when_read`]).
fn is_unknown(
    schema: &om_data::host::CalcSchema,
    unknown: &Unknown,
    (table, record, field): (u64, Option<u64>, u64),
) -> bool {
    let marked = |table, record, field| marked(unknown, (table, record, field));
    let any = unknown
        .values()
        .any(|records| records.values().any(|f| !f.is_empty()));
    om_data::rules::unknown_when_read(schema, (&marked, any), (table, record, field))
}

/// The first id a window's unsaved new record is read under: none of a
/// table's stored records has one this high.
const PENDING_IDS: u64 = 1 << 62;

fn refused(message: &str) -> DataError {
    DataError::with_code(om_data::found::code::UNSUPPORTED, message)
}

impl NativeProvider {
    /// [`DataProvider::preview_new_record`] (module docs), over the
    /// window's unsaved records `pending` when it gives them
    /// ([`DataProvider::preview_new_record_over`]). A `write` naming a
    /// preview (`prepared`) computes that preview again over the cells it
    /// gives now, as after a trigger changed one or vetoed the commit: with
    /// what the preview drew, nothing reserved again, under the same id.
    pub(crate) fn preview_new(
        &mut self,
        write: &ScriptWrite,
        pending: Option<&[PendingRecord]>,
    ) -> Result<Option<NewRecordPreview>, DataError> {
        if write.record.is_some() {
            return Ok(None);
        }
        self.require_writable()?;
        self.require_script_layout_records(write.layout)?;
        let layout = self.decoded_layout(write.layout)?;
        let layout_table = self.read_table_for_layout(&layout)?.id;
        let table = write.table.unwrap_or(layout_table);
        if let Some(route) = self.external.route(table) {
            // A related record of another file's table, or a new record of
            // a layout of another file's table (#43), as `write_record`
            // routes them.
            // Again: the owner's preview this file's id names.
            let again = write.prepared;
            let owned_write;
            let write = match again {
                Some(id) => {
                    owned_write = ScriptWrite {
                        prepared: Some(self.routed_preview(id, table, write.layout)?),
                        ..write.clone()
                    };
                    &owned_write
                }
                None => write,
            };
            // Inside a script's transaction the owner joins it first, so
            // that it previews with the transaction's options (Skip
            // auto-enter), as the write it holds there is computed: a
            // preview before the owner's first write would otherwise enter
            // values, and reserve serial numbers, the commit then skips.
            if self.transaction_kind() == Some(transaction::TransactionKind::Script) {
                self.join_owner_transaction(table, route)?;
            }
            // The window's unsaved records, by the window's file's table ids:
            // this file's overlay only this file's reads (the owner reads
            // them through `CallerSource`), the routed table's rows, by the
            // owner's own id, only the owner's. Two files' tables can share an
            // id (both base tables 0x8001): one file's cells never reach the
            // other's records.
            let usable = pending.filter(|pending| {
                pending.iter().all(|p| {
                    let routed = self.external.route(p.table);
                    match p.record {
                        // A new record of another table: a parent the window
                        // has not created, whose keys the commit may compute.
                        None => p.table == table,
                        Some(_) => routed.is_none() || p.table == table,
                    }
                })
            });
            let root_pending = match usable {
                Some(pending) => {
                    let own: Vec<&PendingRecord> = pending
                        .iter()
                        .filter(|p| self.external.route(p.table).is_none())
                        .collect();
                    let operation = Operation {
                        layout: Some(write.layout),
                        set: CurrentSet::Step(write.privilege_set.clone()),
                        record_number: None,
                    };
                    // Held records of the opened file's tables the owner
                    // reads are read too.
                    let reading: BTreeSet<u64> = self
                        .routed_plan(route)
                        .map(|plan| plan.tables.values().copied().collect())
                        .unwrap_or_default();
                    let unknown = Unknown::new();
                    self.pending_overlay((None, &reading), &own, operation, unknown)?
                }
                None => None,
            };
            let reads = usable.is_some() && root_pending.is_some();
            let rows: Vec<PendingRecord> = usable
                .unwrap_or_default()
                .iter()
                .filter(|p| p.table == table)
                .cloned()
                .collect();
            let owned = self.with_owner_reading_root(route, |owner, wire| {
                owner.require_writable()?;
                Self::check_scalar_cells(
                    owner.catalog.table(wire).ok_or_else(|| {
                        DataError::with_code(
                            om_data::rules::error::FIELD_MISSING,
                            "The owning table definition is unavailable.",
                        )
                    })?,
                    &write.cells,
                )?;
                let reads = match (reads, owner.caller.as_mut(), &root_pending) {
                    (true, Some(caller), Some((root_held, root_unknown))) => {
                        caller.pending = root_held.clone();
                        // The opened file's unknown values, by the owner's
                        // ids for its tables: never the owner's own.
                        // Record ids are the opened file's, as the owner
                        // reads its records.
                        let mut unknown = caller.unknown.clone();
                        for (own, root) in &caller.tables {
                            if let Some(records) = root_unknown.get(root) {
                                let at = unknown.entry(*own).or_default();
                                for (record, fields) in records {
                                    at.entry(*record).or_default().extend(fields);
                                }
                            }
                        }
                        let rows: Vec<PendingRecord> = rows
                            .iter()
                            .map(|row| PendingRecord {
                                table: wire,
                                ..row.clone()
                            })
                            .collect();
                        let rows: Vec<&PendingRecord> = rows.iter().collect();
                        let operation = Operation {
                            layout: None,
                            set: CurrentSet::Account,
                            record_number: None,
                        };
                        let none = BTreeSet::new();
                        owner.pending_overlay((Some(wire), &none), &rows, operation, unknown)?
                    }
                    _ => None,
                };
                // As `routed_write` writes it: no layout of the owner's,
                // the owner session's own set.
                owner.preview_table(wire, None, CurrentSet::Account, (write, reads))
            })?;
            let Some(preview) = owned else {
                return Ok(None);
            };
            let id = match again {
                Some(id) => id,
                None => self.previews.insert(Preview::Routed {
                    route,
                    table,
                    layout: write.layout,
                    owner: preview.id,
                }),
            };
            return Ok(Some(NewRecordPreview {
                id,
                values: preview.values,
            }));
        }
        self.refuse_imported_table(table)?;
        // The window's unsaved records of this file's tables; a new one
        // only of the written row's table (module docs).
        let reads = match pending {
            Some(pending)
                if pending.iter().all(|p| {
                    self.external.route(p.table).is_none()
                        && (p.record.is_some() || p.table == table)
                }) =>
            {
                let all: Vec<&PendingRecord> = pending.iter().collect();
                let operation = Operation {
                    layout: Some(write.layout),
                    set: CurrentSet::Step(write.privilege_set.clone()),
                    record_number: None,
                };
                let (none, unknown) = (BTreeSet::new(), Unknown::new());
                self.pending_overlay((Some(table), &none), &all, operation, unknown)?
            }
            _ => None,
        };
        let set = CurrentSet::Step(write.privilege_set.clone());
        self.preview_table(table, Some(write.layout), set, (write, reads))
    }

    /// `pending`, records of this provider's tables, as a held-record
    /// overlay over the open transaction's holdings, each as its commit
    /// would compute it ([`RecordPlan::pending_row`], #1063): a record the
    /// transaction holds new (the window's record session) from its held
    /// cells, the serial numbers it took and its clock and UUID, with the
    /// window's cells over them; an existing one from its stored record (or
    /// what the transaction holds of it) with the window's cells; a row not
    /// created yet from its cells (a begun row's: what its own preview
    /// entered among them). Records of other tables than `table` (the
    /// written row's) come first, then its rows, each in the order given,
    /// each reading the ones before it as computed. `operation`: the
    /// window's layout and current set, which a record the transaction does
    /// not hold is computed with. `unknown`: values already unknown (the
    /// opened file's, for an owner's rows). With the overlay (`None`:
    /// nothing to overlay), the fields of these records whose values their
    /// commits may not store, field by field: each record's own
    /// ([`RecordPlan::pending_row`]) and what it reads of the ones before
    /// it ([`RecordPlan::masked_fields`]). `None` when one cannot be read
    /// at all (one reading a record after it, an imported one, a typed value
    /// not of its type), so the preview reads no other record.
    fn pending_overlay(
        &mut self,
        table: (Option<u64>, &BTreeSet<u64>),
        pending: &[&PendingRecord],
        operation: Operation,
        unknown: Unknown,
    ) -> Result<Option<Pending>, DataError> {
        Ok(self
            .pending_computed(table, pending, operation, unknown, None)?
            .map(|(pending, _)| pending))
    }

    /// [`Self::pending_overlay`], with the row and unknown fields of the
    /// `own`th record given, as computed (`None` when no record is named).
    #[allow(clippy::type_complexity)]
    fn pending_computed(
        &mut self,
        (table, reading): (Option<u64>, &BTreeSet<u64>),
        pending: &[&PendingRecord],
        operation: Operation,
        mut unknown: Unknown,
        own: Option<usize>,
    ) -> Result<Option<(Pending, Option<(BTreeMap<u64, String>, BTreeSet<u64>)>)>, DataError> {
        // The transaction's other held records these read, directly or
        // through each other, as their commit computes them: first, in the
        // order the commit computes them, not their held cells over the
        // stored row (whose calculations are stale).
        let held = self.held_read_by(pending, table.iter().chain(reading).copied())?;
        let ordered = held.iter().chain(pending.iter().copied());
        let mut ordered: Vec<(usize, &PendingRecord)> = ordered.enumerate().collect();
        let own = own.map(|own| own + held.len());
        ordered.sort_by_key(|(_, p)| Some(p.table) == table);
        // A record reading one computed after it would read it uncomputed,
        // as Commit Transaction refuses (`prepare_held`).
        for (at, (_, record)) in ordered.iter().enumerate() {
            let reads = self.table_plan(record.table)?.1.related_tables();
            if ordered[at + 1..]
                .iter()
                .any(|(_, later)| reads.contains(&later.table))
            {
                return Ok(None);
            }
        }
        let mut overlay = self
            .held_overlay_with(None)?
            .map(|held| (*held).clone())
            .unwrap_or_default();
        let mut any = overlay != HeldRecords::default();
        let mut named = None;
        for (at, record) in ordered {
            let Some((id, row, fields)) =
                self.pending_one(record, at, (&overlay, &unknown), &operation)?
            else {
                return Ok(None);
            };
            if Some(at) == own {
                named = Some((row.clone(), fields.clone()));
            }
            if !fields.is_empty() {
                let at = unknown.entry(record.table).or_default();
                at.entry(id).or_default().extend(fields);
            }
            overlay.hold(record.table, id, row);
            any = true;
        }
        let pending = (any.then(|| std::sync::Arc::new(overlay)), unknown);
        Ok(Some((pending, named)))
    }

    /// The records the open transaction holds that `pending` or the tables
    /// `reading` read, directly or through each other (by the tables their
    /// formulas read), and that `pending` does not name: each with no cells
    /// of the window's.
    fn held_read_by(
        &mut self,
        pending: &[&PendingRecord],
        reading: impl Iterator<Item = u64>,
    ) -> Result<Vec<PendingRecord>, DataError> {
        let held = self.held_in_order();
        if held.is_empty() {
            return Ok(Vec::new());
        }
        let mut tables: BTreeSet<u64> = BTreeSet::new();
        let mut next: BTreeSet<u64> = pending.iter().map(|p| p.table).chain(reading).collect();
        while !next.is_empty() {
            let mut reads = BTreeSet::new();
            for table in &next {
                // Imported tables of a routed owner's plan are supplied by
                // CallerSource, not transactions owned here. Do not try to
                // compute a peer's provisional rows as local records.
                let caller = self.routed_plan.as_ref().is_some_and(|plan| {
                    plan.tables.contains_key(table)
                        || plan
                            .others
                            .iter()
                            .any(|other| other.tables.contains_key(table))
                });
                if !caller && self.external.route(*table).is_none() {
                    reads.extend(self.table_plan(*table)?.1.related_tables());
                }
            }
            tables.extend(std::mem::take(&mut next));
            next = reads.difference(&tables).copied().collect();
        }
        let named: BTreeSet<(u64, i64)> = pending
            .iter()
            .filter_map(|p| Some((p.table, p.record?)))
            .collect();
        Ok(held
            .into_iter()
            .filter_map(|(table, record)| {
                let id = i64::try_from(record).ok()?;
                (tables.contains(&table) && !named.contains(&(table, id))).then(|| PendingRecord {
                    table,
                    record: Some(id),
                    ..Default::default()
                })
            })
            .collect())
    }

    /// This file's held records that a related file's commit reads (the
    /// tables `plan` names), as this file's commit will write them, for that
    /// commit, which comes first (#1006, `with_owner_reading_committed_root`):
    /// computed as a preview computes the window's records
    /// ([`Self::pending_overlay`]), their values this commit may store
    /// otherwise unknown, by the owner's ids for the tables. Where they
    /// cannot be computed, every value of those held records is unknown.
    /// `elsewhere`: the related files' records the save holds, their values
    /// unknown (`OwnerTransaction::held_unknown`, #1167): what this file's
    /// held records compute from them is unknown too.
    pub(crate) fn root_as_committed(
        &mut self,
        plan: &crate::external::OwnerPlan,
        elsewhere: Unknown,
    ) -> Result<
        (
            Option<std::sync::Arc<HeldRecords>>,
            crate::related_owner::CallerUnknown,
        ),
        DataError,
    > {
        let mut unknown = crate::related_owner::CallerUnknown::new();
        // A transaction holding only deletions still hides those records.
        let held = self.held_in_order();
        let reading: BTreeSet<u64> = plan.tables.values().copied().collect();
        let operation = Operation {
            layout: None,
            set: CurrentSet::Account,
            record_number: None,
        };
        let computed = self.pending_overlay((None, &reading), &[], operation, elsewhere)?;
        let owner_of = |root: u64| {
            plan.tables
                .iter()
                .find(|(_, at)| **at == root)
                .map(|(own, _)| *own)
        };
        let Some((pending, root_unknown)) = computed else {
            // Read as held, deletions included, and every value of a held
            // record unknown.
            let raw = self.held_overlay_with(None)?;
            let schema = self.calc_schema_cached()?;
            for (table, record) in held {
                let (Some(own), Some(def)) = (owner_of(table), schema.table(table)) else {
                    continue;
                };
                let fields = def.fields.keys().copied().collect();
                unknown.entry(own).or_default().insert(record, fields);
            }
            return Ok((raw, unknown));
        };
        for (root, records) in root_unknown {
            if let Some(own) = owner_of(root) {
                unknown.entry(own).or_default().extend(records);
            }
        }
        Ok((pending, unknown))
    }

    /// [`DataProvider::compute_pending`] (#1104): the window's own record
    /// `write` names, with its cells, computed as [`Self::pending_overlay`]
    /// computes a pending record, after the window's other unsaved records
    /// `others` (as a preview reads them: this file's tables, a new one
    /// only of the record's table). Its row is every cell its commit would
    /// store; its unknown fields are what [`RecordPlan::pending_row`] marks
    /// and what it reads of the others' unknown values. Nothing is sent or
    /// reserved.
    pub(crate) fn pending_values(
        &mut self,
        write: &ScriptWrite,
        others: &[PendingRecord],
    ) -> Result<Option<om_data::PendingValues>, DataError> {
        self.require_script_layout_records(write.layout)?;
        let layout = self.decoded_layout(write.layout)?;
        let table = match write.table {
            Some(table) => table,
            None => self.read_table_for_layout(&layout)?.id,
        };
        let usable = others.iter().all(|p| {
            self.external.route(p.table).is_none() && (p.record.is_some() || p.table == table)
        });
        // A write naming a preview takes that preview's context, which this
        // does not compute with.
        if self.external.route(table).is_some() || !usable || write.prepared.is_some() {
            return Ok(None);
        }
        let own = PendingRecord {
            table,
            record: write.record,
            cells: write.cells.clone(),
            typed: write.typed.clone(),
        };
        // The record itself is computed last, from the write's cells.
        let mut all: Vec<&PendingRecord> = others
            .iter()
            .filter(|p| p.record.is_none() || (p.table, p.record) != (table, write.record))
            .collect();
        all.push(&own);
        let operation = Operation {
            layout: Some(write.layout),
            set: CurrentSet::Step(write.privilege_set.clone()),
            record_number: write.record_number,
        };
        let at = all.len() - 1;
        let Some((_, Some((row, unknown)))) = self.pending_computed(
            (Some(table), &BTreeSet::new()),
            &all,
            operation,
            Unknown::new(),
            Some(at),
        )?
        else {
            return Ok(None);
        };
        let mut values: BTreeMap<u64, Option<String>> = row
            .into_iter()
            .map(|(cell, text)| (cell, Some(text)))
            .collect();
        for (cell, value) in &write.cells {
            if value.is_none() {
                values.entry(*cell).or_insert(None);
            }
        }
        Ok(Some(om_data::PendingValues { values, unknown }))
    }

    /// Pending record `record` (the `at`th given) computed as its commit
    /// would compute it ([`Self::pending_overlay`]), over `overlay` and the
    /// values `unknown` already: the id it is read under, its row and its
    /// unknown fields. `None` when it cannot be read at all.
    #[allow(clippy::type_complexity)]
    fn pending_one(
        &mut self,
        record: &PendingRecord,
        at: usize,
        (overlay, unknown): (&HeldRecords, &Unknown),
        operation: &Operation,
    ) -> Result<Option<(u64, BTreeMap<u64, String>, BTreeSet<u64>)>, DataError> {
        use crate::transaction::HeldBasis;
        use om_data::rules::Written;
        let (_, plan) = self.table_plan(record.table)?;
        let mut cells = record.cells.clone();
        for cell in &record.typed {
            if let Some(Some(text)) = cells.get_mut(cell) {
                match plan.typed_text(*cell, text) {
                    Ok(stored) => *text = stored,
                    Err(_) => return Ok(None),
                }
            }
        }
        let window = |provider: &mut Self| {
            let origin = Origin {
                layout: operation.layout,
                set: operation.set.clone(),
                record_number: operation.record_number,
                prepared: None,
            };
            provider.origin_context(&plan, origin)
        };
        let stored = |provider: &mut Self, id: u64| -> Result<_, DataError> {
            Ok(provider
                .read_record(record.table, id)?
                .map(|node| records::text_values(&records::stored_record(id, &node))))
        };
        // The rules its write commits it by: the open transaction's
        // options, else a script's write's (`hold_script`).
        let entry_for = |provider: &Self, create: bool| match &provider.transaction {
            Some(transaction) => Entry {
                script: true,
                keep_written: create,
                skip_data_entry_validation: transaction.skips_validation(),
                skip_auto_enter: transaction.skips_auto_enter(),
                mapped_cells: None,
            },
            None => records::script_entry(false, create),
        };
        // A held record the window changes is written again: that write
        // replaces its context (`hold`), unless it was written from a
        // preview.
        let changed = !cells.is_empty();
        // Its mode, stored row, changes, serial numbers, a clock and UUID
        // its commit keeps, context, rules, id, and whether its commit
        // keeps this clock and UUID, and this host clock.
        let (mode, existing, changes, serials, stamp, context, entry, which, id, kept) =
            match record.record {
                Some(id) => {
                    let id = u64::try_from(id).map_err(|_| DataError::new("Invalid record ID."))?;
                    match self.held_basis((record.table, id)) {
                        Some(HeldBasis::New {
                            cells: held,
                            serials,
                            stamp,
                            context,
                            entry,
                            previewed,
                        }) => {
                            let replaced = changed && !previewed;
                            let context = match replaced {
                                true => window(self)?,
                                false => context,
                            };
                            let entry = match changed {
                                true => entry_for(self, true),
                                false => entry,
                            };
                            let kept = (stamp.is_some(), !replaced);
                            let mut changes = held;
                            changes.extend(cells);
                            let which = Written::held(WriteMode::Create, id);
                            let (mode, base) = (WriteMode::Create, BTreeMap::new());
                            (
                                mode, base, changes, serials, stamp, context, entry, which, id,
                                kept,
                            )
                        }
                        Some(HeldBasis::Edit {
                            values,
                            context,
                            entry,
                        }) => {
                            let Some(base) = stored(self, id)? else {
                                return Ok(None);
                            };
                            let (context, entry) = match changed {
                                true => (window(self)?, entry_for(self, false)),
                                false => (context, entry),
                            };
                            let mut changes = values;
                            changes.extend(cells);
                            let which = Written::Stored(id);
                            let (mode, serials) = (WriteMode::Update, BTreeMap::new());
                            let kept = (false, !changed);
                            (
                                mode, base, changes, serials, None, context, entry, which, id, kept,
                            )
                        }
                        Some(HeldBasis::Other) => return Ok(None),
                        None => {
                            let Some(base) = stored(self, id)? else {
                                return Ok(None);
                            };
                            let context = window(self)?;
                            let entry = entry_for(self, false);
                            let which = Written::Stored(id);
                            let (mode, serials) = (WriteMode::Update, BTreeMap::new());
                            (
                                mode,
                                base,
                                cells,
                                serials,
                                None,
                                context,
                                entry,
                                which,
                                id,
                                (false, false),
                            )
                        }
                    }
                }
                None => {
                    let context = window(self)?;
                    let entry = entry_for(self, true);
                    // Under an id no stored record has.
                    let id = PENDING_IDS + at as u64;
                    let (mode, base, serials) =
                        (WriteMode::Create, BTreeMap::new(), BTreeMap::new());
                    let which = Written::New(None);
                    (
                        mode,
                        base,
                        cells,
                        serials,
                        None,
                        context,
                        entry,
                        which,
                        id,
                        (false, false),
                    )
                }
            };
        let mut environment = self.rule_environment(record.table, &plan, context)?;
        environment.serials = serials;
        if let Some(stamp) = &stamp {
            stamp.apply(&mut environment);
        }
        environment.held = Some(std::sync::Arc::new(overlay.clone()));
        // What it evaluates from the records before it that their
        // commits may not store is unknown in it too.
        let before = |t, r, f| is_unknown(plan.schema(), unknown, (t, r, f));
        let run = (mode, entry, which);
        let reads = (&existing, &changes);
        let (row, fields) =
            self.pending_record(&plan, run, reads, &environment, (kept, &before))?;
        Ok(Some((id, row, fields)))
    }

    /// The preview of a new record of this file's base table `table`
    /// written through `layout` with `set` current; `None` when a value
    /// the user typed does not read as its field's type (the write
    /// refuses it).
    /// With `reads` (the window's unsaved records are known), its lookups
    /// and related records are read through them (`Some(None)`: there are
    /// none to overlay); without, it reads no other record.
    fn preview_table(
        &mut self,
        table: u64,
        layout: Option<u32>,
        set: CurrentSet,
        (write, reads): (&ScriptWrite, Option<Pending>),
    ) -> Result<Option<NewRecordPreview>, DataError> {
        self.require_action(table, om_data::host::security::RecordAction::Create)?;
        let (_, plan) = self.table_plan(table)?;
        let mut cells = write.cells.clone();
        for cell in &write.typed {
            if let Some(Some(text)) = cells.get_mut(cell) {
                match plan.typed_text(*cell, text) {
                    Ok(stored) => *text = stored,
                    Err(_) => return Ok(None),
                }
            }
        }
        // As the write's rules: a script's new record, held with the open
        // transaction's options when there is one.
        let entry = match &self.transaction {
            Some(transaction) => Entry {
                script: true,
                keep_written: true,
                skip_data_entry_validation: transaction.skips_validation(),
                skip_auto_enter: transaction.skips_auto_enter(),
                mapped_cells: None,
            },
            None => records::script_entry(write.skip_validation, true),
        };
        // Again, a preview's own context, serial numbers, clock and UUID
        // (refused when it is not this record's, or is held already).
        let origin = Origin {
            layout,
            set,
            record_number: write.record_number,
            prepared: write.prepared,
        };
        let context = self.origin_context(&plan, origin)?;
        let mut environment = self.rule_environment(table, &plan, context)?;
        let on_creation = plan
            .serials_for(WriteMode::Create, entry)
            .into_iter()
            .filter(|serial| !serial.on_commit);
        match write.prepared {
            Some(id) => {
                let fields = on_creation.map(|serial| serial.field).collect();
                let reuse = self.preview_reuse(id, table, &fields)?;
                environment.serials = reuse.serials;
                reuse.stamp.apply(&mut environment);
            }
            None => environment.serials = self.reserve_serials(table, on_creation)?,
        }
        // Without the window's unsaved records, nothing read from another
        // record: only the host's stored one could be read here.
        let others = reads.is_some();
        let (held, mut unknown) = reads.unwrap_or_default();
        // Pending rows and the final preview share the caller's peer mask:
        // its raw held cells are not final computed values (#1198).
        if let Some(caller) = &self.caller {
            for (table, records) in &caller.unknown {
                for (record, fields) in records {
                    unknown
                        .entry(*table)
                        .or_default()
                        .entry(*record)
                        .or_default()
                        .extend(fields);
                }
            }
        }
        environment.held = held;
        // Left out besides: what reads a value of the window's records that
        // their commits may not store (#1063), on the records it reads.
        let fields = plan.preview_fields(others);
        let unknown = |t, r, f| is_unknown(plan.schema(), &unknown, (t, r, f));
        let run = (entry, others);
        let mut values =
            self.preview_record(&plan, run, &cells, &environment, (&fields, &unknown))?;
        values.retain(|cell, value| cells.get(cell) != Some(value));
        let id = match write.prepared {
            Some(id) => id,
            None => self.previews.insert(Preview::Own(Snapshot {
                table,
                layout,
                serials: std::mem::take(&mut environment.serials),
                stamp: Stamp::of(&environment),
                context: environment.context,
            })),
        };
        Ok(Some(NewRecordPreview { id, values }))
    }

    /// [`DataProvider::discard_prepared`].
    pub(crate) fn discard_preview(&mut self, id: PreparedId) {
        if let Some(Preview::Routed { route, owner, .. }) = self.previews.held.remove(&id) {
            let _ = self.external.with_owner(route, |provider, _| {
                provider.discard_preview(owner);
                Ok(())
            });
        }
    }

    /// The owner's id of routed preview `id` of imported table `table`,
    /// written through `layout`.
    pub(crate) fn routed_preview(
        &self,
        id: PreparedId,
        table: u64,
        layout: u32,
    ) -> Result<PreparedId, DataError> {
        match self.previews.held.get(&id) {
            Some(Preview::Routed {
                table: previewed,
                layout: through,
                owner,
                ..
            }) if *previewed == table && *through == layout => Ok(*owner),
            Some(_) => Err(refused(
                "The new record's preview is of another record. No record was written.",
            )),
            None => Err(missing()),
        }
    }

    /// Preview `id` of a new record of `table` written through `layout`.
    fn snapshot(
        &self,
        id: PreparedId,
        table: u64,
        layout: Option<u32>,
    ) -> Result<&Snapshot, DataError> {
        match self.previews.held.get(&id) {
            Some(Preview::Own(snapshot))
                if snapshot.table == table && snapshot.layout == layout =>
            {
                Ok(snapshot)
            }
            Some(_) => Err(refused(
                "The new record's preview is of another record. No record was written.",
            )),
            None => Err(missing()),
        }
    }

    /// The operation's context a write of `origin` naming a preview
    /// computes with ([`Self::origin_context`]): the preview's, with the
    /// record number the write gives, `None` included (#1084). The window
    /// numbers the record afresh at each commit and gives none where the
    /// commit is out of the measured shape (a trigger changed a match key
    /// since the preview): a formula reading it is then refused, as without
    /// a preview, rather than given the preview's number. A transaction's
    /// later writes of the held record keep the number its creation gave
    /// (`hold`).
    pub(crate) fn preview_context(
        &self,
        id: PreparedId,
        table: u64,
        origin: &Origin,
    ) -> Result<WriteContext, DataError> {
        let mut context = self.snapshot(id, table, origin.layout)?.context.clone();
        context.record_number = origin.record_number;
        Ok(context)
    }

    /// What the write creating the record of preview `id` of `table`
    /// reuses: the serial numbers among `on_creation` (the fields the
    /// record's rules take on creation now) the preview reserved, and its
    /// clock and UUID. Refused while the open transaction holds a record
    /// of it.
    pub(crate) fn preview_reuse(
        &self,
        id: PreparedId,
        table: u64,
        on_creation: &BTreeSet<u64>,
    ) -> Result<Reuse, DataError> {
        let snapshot = match self.previews.held.get(&id) {
            Some(Preview::Own(snapshot)) if snapshot.table == table => snapshot,
            Some(_) => {
                return Err(refused(
                    "The new record's preview is of another record. No record was written.",
                ))
            }
            None => return Err(missing()),
        };
        if self.holds_preview(id) {
            return Err(refused(
                "The new record's preview was written already in this transaction. No \
                 record was written.",
            ));
        }
        Ok(Reuse {
            serials: snapshot
                .serials
                .iter()
                .filter(|(field, _)| on_creation.contains(field))
                .map(|(field, value)| (*field, value.clone()))
                .collect(),
            stamp: snapshot.stamp.clone(),
        })
    }

    /// What held new record `cells` of `table` shows before its commit
    /// ([`Created`]): its rules computed as a preview computes them, with
    /// the serial numbers it took on creation (`serials`), the clock and
    /// UUID `stamp` (drawn now when it has none), and what its operation
    /// supplied (`context`). Like a preview, it reads no other record:
    /// lookups, related reads, a `Random` draw and the fields computed from
    /// one are left to the commit. Nothing is written or reserved.
    pub(crate) fn created_values(
        &mut self,
        (table, plan): (u64, &om_data::rules::RecordPlan),
        entry: Entry,
        (cells, serials): (&BTreeMap<u64, Option<String>>, &BTreeMap<u64, String>),
        context: WriteContext,
        stamp: Option<&Stamp>,
    ) -> Result<Created, DataError> {
        let mut environment = self.rule_environment(table, plan, context)?;
        environment.serials = serials.clone();
        if let Some(stamp) = stamp {
            stamp.apply(&mut environment);
        }
        // Its serial numbers are entered by its rules, not given, as the
        // commit plans it (`held_serials`).
        let given: BTreeMap<u64, Option<String>> = cells
            .iter()
            .filter(|(cell, value)| value.is_none() || serials.get(cell) != value.as_ref())
            .map(|(cell, value)| (*cell, value.clone()))
            .collect();
        let fields = plan.preview_fields(false);
        let none = |_, _, _| false;
        let reads = (&fields, &none as om_data::rules::UnknownValue<'_>);
        let values = self.preview_record(plan, (entry, false), &given, &environment, reads)?;
        Ok(Created {
            stamp: Stamp::of(&environment),
            values,
        })
    }

    /// Preview `id`'s record was created, or may have been: its serial
    /// numbers are not reused.
    pub(crate) fn use_up_preview(&mut self, id: PreparedId) {
        self.previews.held.remove(&id);
    }
}

fn missing() -> DataError {
    refused(
        "The new record's preview is no longer available (it was written, discarded, or the \
         file reconnected). No record was written.",
    )
}
