//! Container writes of the desktop client, as FileMaker Pro makes them
//! (containers.md 3, 5): data put into a container field is stored in the
//! table's library with op88 before the record is committed, a value of
//! the same table's library takes one more reference with op87, and the
//! client releases (op87 -1) what a record no longer holds: the stored
//! value a commit replaces or clears, and every value of a deleted record.
//! The server never counts references itself (5.1).
//!
//! Releases happen once the commit or delete succeeded, so a failure never
//! loses data; a release that fails only leaves an orphan, which FileMaker
//! tolerates too (5.2).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use log::warn;
use om_data::calc::eval::{Pending, StoredContainer};
use om_data::calc::{CalcError, Container};
use om_data::containers::{self, MemberInfo, LIBRARY};
use om_data::host::{CalcNeed, MemberKey};
use om_data::rules::{
    ContainerReader, Entry, PreparedRecord, RecordEnvironment, RecordPlan, WriteMode,
};
use om_data::DataError;
use om_hbam::{DataType, OwnedNode, Table};
use om_protocol::container::{COUNT_FAILED, LIBRARY_OBJECT_EXISTS};

use crate::{failure, key_bytes, NativeProvider};

/// `[T, 1f, 05, M]`.
fn member_path(table: u64, member: &[u8]) -> Vec<Vec<u8>> {
    vec![
        key_bytes(table),
        vec![LIBRARY],
        vec![containers::keys::MEMBERS],
        member.to_vec(),
    ]
}

/// The members the container fields of `table` name in a record's texts
/// (by cell, repetition n of field F being `F * 256 + n`), of `cells` only
/// when given.
pub(crate) fn references(
    table: &Table,
    texts: &BTreeMap<u64, String>,
    cells: Option<&[u64]>,
) -> Vec<Vec<u8>> {
    texts
        .iter()
        .filter(|(cell, _)| cells.map_or(true, |c| c.contains(cell)))
        .filter(|(cell, _)| {
            let field = table.field(**cell).or_else(|| table.field(**cell >> 8));
            field.is_some_and(|f| f.data_type == DataType::Container)
        })
        .filter_map(|(_, text)| containers::reference(text))
        .collect()
}

/// What a write's rules read of the libraries (containers.md 6.1), read as
/// they ask for it: a calculation reading a container field gets the
/// member's header, a function reading its streams the data, and nothing
/// else is downloaded. The rules run without the connection, so what was
/// not read yet answers 1224 ([`ContainerReader::pending`]) and the first
/// such read of a run is noted: [`NativeProvider::prepare_record`]
/// downloads it and runs them again. Only the first, because a failed read
/// is `?` to the rest of the formula, which may then take a branch the
/// complete data would not; up to the first, the run is the complete one.
/// What was read stays for the write's later runs.
#[derive(Default)]
struct WriteContainers {
    read: Mutex<Reads>,
}

#[derive(Default)]
struct Reads {
    /// Headers; `None`: the library has no such member.
    members: HashMap<MemberKey, Option<Arc<MemberInfo>>>,
    /// Data; `None`: the member holds none that reads.
    data: HashMap<MemberKey, Option<Arc<StoredContainer>>>,
    /// The first read the current run lacked.
    need: Option<CalcNeed>,
    pending: Pending,
}

impl WriteContainers {
    fn reads(&self) -> MutexGuard<'_, Reads> {
        self.read.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The first read the run lacked, taken.
    fn take_need(&self) -> Option<CalcNeed> {
        self.reads().need.take()
    }

    fn put_member(&self, key: MemberKey, info: Option<Arc<MemberInfo>>) {
        self.reads().members.insert(key, info);
    }

    fn put_data(&self, key: MemberKey, data: Option<Arc<StoredContainer>>) {
        self.reads().data.insert(key, data);
    }
}

impl Reads {
    /// Note `need` if the run lacked nothing before, count the read as
    /// pending, and answer it with 1224.
    fn lack(&mut self, need: CalcNeed) -> CalcError {
        self.pending.reads += 1;
        if self.need.is_none() {
            self.need = Some(need);
            self.pending.needs += 1;
        }
        CalcError::from_code(1224).expect("nonzero")
    }
}

impl ContainerReader for WriteContainers {
    /// The desktop stores data before it writes (op88): a cell holds a
    /// member's reference or text.
    fn value(&self, _table: u64, _text: &str) -> Option<Container> {
        None
    }

    fn data(&self, table: u64, member: &[u8]) -> Option<StoredContainer> {
        let key: MemberKey = (table, member.to_vec());
        let data = self.reads().data.get(&key).cloned().flatten()?;
        Some(StoredContainer::clone(&data))
    }

    fn member(&self, table: u64, member: &[u8]) -> Result<Option<Arc<MemberInfo>>, CalcError> {
        let mut reads = self.reads();
        let key: MemberKey = (table, member.to_vec());
        match reads.members.get(&key) {
            Some(info) => Ok(info.clone()),
            None => Err(reads.lack(CalcNeed::Member {
                table,
                member: member.to_vec(),
            })),
        }
    }

    fn member_data(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<StoredContainer>>, CalcError> {
        let mut reads = self.reads();
        let key: MemberKey = (table, member.to_vec());
        match reads.data.get(&key) {
            Some(data) => Ok(data.clone()),
            None => Err(reads.lack(CalcNeed::MemberData {
                table,
                member: member.to_vec(),
            })),
        }
    }

    fn pending(&self) -> Pending {
        self.reads().pending
    }
}

/// How many times a write is computed again with match keys it computed
/// itself (#1035, [`om_data::rules::RelatedNeeds`]): one per key that
/// depends on what another such key reads.
const KEY_ROUNDS: usize = 4;
impl NativeProvider {
    /// [`RecordPlan::prepare_entry`] with the container data the write's
    /// stored and auto-enter calculations and validations read (they see a
    /// container's data, script oracle s34-containers-calc): only the
    /// members they read, the header for what reads a field and the data
    /// for what reads its streams, each downloaded once, through the
    /// connection's cache ([`WriteContainers`]). A run that lacked a read
    /// is discarded, whatever it computed, and the rules run again once it
    /// is downloaded; what cannot be downloaded fails the write.
    pub(crate) fn prepare_record(
        &mut self,
        plan: &RecordPlan,
        (mode, entry, which): (WriteMode, Entry, om_data::rules::Written),
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
    ) -> Result<PreparedRecord, DataError> {
        // Creates, edits, imports, script writes and Commit Transaction all
        // come here, on the provider that owns the table: a field the
        // account's custom set gives view-only access is not changed by the
        // write itself (201, "Field cannot be modified"). What the rules
        // enter themselves is not among `changes`.
        if let Some(access) = &self.account_info.set_access {
            let table = om_hbam::slot::uint_segment(plan.table());
            let modifies = |field| access.modifies(&table, &om_hbam::slot::uint_segment(field));
            if plan.unmodifiable_change(changes, modifies).is_some() {
                // FileMaker Pro names no field (#137's reading).
                return Err(DataError::with_code(201, crate::FIELD_NOT_MODIFIABLE));
            }
        }
        // A custom set's formula limiting the action, on the record: the
        // stored values for an edit, the values the write sets for a new
        // record (the host decides again on the whole record at commit).
        {
            use om_data::host::security::RecordAction;
            let (action, row) = match mode {
                WriteMode::Update => (RecordAction::Edit, existing.clone()),
                _ => (
                    RecordAction::Create,
                    changes
                        .iter()
                        .filter_map(|(cell, text)| Some((*cell, text.clone()?)))
                        .collect(),
                ),
            };
            self.require_limit(plan.table(), action, &row)?;
        }
        // Written for the opened file's commit, which comes after this one
        // (#1006): a value of its held records that commit may store
        // otherwise is not read (`CallerReads::unknown`).
        let unknown = self.caller.as_ref().map(|caller| caller.unknown.clone());
        let unknown = unknown.filter(|unknown| !unknown.is_empty());
        let compute = |plan: &RecordPlan, environment: &RecordEnvironment| match &unknown {
            Some(unknown) => {
                let marked = |t, r, f| crate::preview::marked(unknown, (t, r, f));
                let write = (mode, existing, changes);
                plan.prepare_entry_over(write, environment, entry, &marked)
            }
            None => plan.prepare_entry(mode, existing, changes, environment, entry),
        };
        let run = (mode, entry, which);
        let reads = (existing, changes);
        self.run_rules(plan, run, reads, environment, (true, true), compute)
    }

    /// The values the rules give a new record of `plan` from `changes`,
    /// validation left to its write ([`RecordPlan::preview_entry`] of
    /// `fields`, [`RecordPlan::preview_fields`]), with the record's own
    /// container data as [`Self::prepare_record`] reads it, and, with
    /// `others`, its lookups and related records, through the window's
    /// unsaved records `environment` holds. The serial numbers
    /// `environment` lacks are not reserved: their fields are left out
    /// (preview.rs).
    pub(crate) fn preview_record(
        &mut self,
        plan: &RecordPlan,
        (entry, others): (Entry, bool),
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
        (fields, unknown): (&BTreeSet<u64>, om_data::rules::UnknownValue<'_>),
    ) -> Result<BTreeMap<u64, Option<String>>, DataError> {
        let existing = BTreeMap::new();
        // Not evaluated: what reads a value `unknown` says its commit may
        // not store, on the records read (`masked_fields`).
        let compute = |plan: &RecordPlan, environment: &RecordEnvironment| {
            let masked = plan.masked_fields(unknown, &environment.lookups);
            let fields = fields.difference(&masked).copied().collect();
            plan.preview_entry(
                WriteMode::Create,
                &existing,
                changes,
                environment,
                entry,
                &fields,
            )
        };
        let run = (WriteMode::Create, entry, om_data::rules::Written::New(None));
        let reads = (&existing, changes);
        self.run_rules(plan, run, reads, environment, (others, false), compute)
    }

    /// [`RecordPlan::pending_row`] of a record the window holds unsaved
    /// (preview.rs, #1063), over the reads [`Self::prepare_record`] makes
    /// (lookups, related records, container data), nothing reserved.
    /// `kept`: whether its commit keeps `environment`'s clock and UUID, and
    /// its host clock; `unknown`: which values of other records their
    /// commits may not store. Its row, and the fields of it that are
    /// unknown.
    #[allow(clippy::type_complexity)]
    pub(crate) fn pending_record(
        &mut self,
        plan: &RecordPlan,
        run: (WriteMode, Entry, om_data::rules::Written),
        (existing, changes): (&BTreeMap<u64, String>, &BTreeMap<u64, Option<String>>),
        environment: &RecordEnvironment,
        (kept, unknown): ((bool, bool), om_data::rules::UnknownValue<'_>),
    ) -> Result<(BTreeMap<u64, String>, BTreeSet<u64>), DataError> {
        let (mode, entry, _) = run;
        let compute = |plan: &RecordPlan, environment: &RecordEnvironment| {
            plan.pending_row(mode, existing, changes, environment, (entry, kept, unknown))
        };
        let reads = (existing, changes);
        self.run_rules(plan, run, reads, environment, (true, false), compute)
    }

    /// Discover a held record's dependencies without final preparation or
    /// serial reservation. The same bounded match-key reads as a write are
    /// used; provisional computed values are never published to peers.
    pub(crate) fn held_reads_ready(
        &mut self,
        plan: &RecordPlan,
        run: (WriteMode, Entry, om_data::rules::Written),
        (existing, changes): (&BTreeMap<u64, String>, &BTreeMap<u64, Option<String>>),
        environment: &RecordEnvironment,
        unknown: &crate::preview::Unknown,
    ) -> Result<bool, DataError> {
        let (mode, entry, which) = run;
        let mut unknown = unknown.clone();
        // Own cells are computed together by this record's rule plan.
        if let Some(records) = unknown.get_mut(&plan.table()) {
            if let Some(record) = match which {
                om_data::rules::Written::New(record) => record,
                om_data::rules::Written::Stored(record) => Some(record),
                om_data::rules::Written::Unknown => None,
            } {
                records.remove(&record);
            }
        }
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
        let unsettled = unknown
            .values()
            .flat_map(|records| records.values())
            .any(|fields| !fields.is_empty());
        if !unsettled {
            // No other held computation can affect this write. Leave its
            // keys, including volatile ones, to its one final preparation.
            return Ok(true);
        }
        let compute = |plan: &RecordPlan, environment: &RecordEnvironment| {
            let marked = |t, r, f| crate::preview::marked(&unknown, (t, r, f));
            let unknown = |t, r, f| {
                om_data::rules::unknown_when_read(plan.schema(), (&marked, unsettled), (t, r, f))
            };
            plan.held_reads_ready((mode, existing, changes), environment, entry, &unknown)
        };
        self.run_rules(
            plan,
            run,
            (existing, changes),
            environment,
            (true, false),
            compute,
        )
    }

    /// `compute` of the record's rules over what they read: container data;
    /// with `read`, lookups and the related records read by match keys the
    /// write computes ([`Self::prepare_record`]); with `reserve`, the serial
    /// numbers not taken yet, reserved.
    fn run_rules<T>(
        &mut self,
        plan: &RecordPlan,
        (mode, entry, which): (WriteMode, Entry, om_data::rules::Written),
        (existing, changes): (&BTreeMap<u64, String>, &BTreeMap<u64, Option<String>>),
        environment: &RecordEnvironment,
        (read, reserve): (bool, bool),
        compute: impl Fn(&RecordPlan, &RecordEnvironment) -> Result<T, DataError>,
    ) -> Result<T, DataError> {
        // Each run but the last downloads one header or one member's data
        // the runs before did not: two per member the record names.
        let written = changes
            .iter()
            .filter_map(|(cell, text)| Some((cell, text.as_ref()?)));
        let named: BTreeSet<Vec<u8>> = plan
            .container_cells(existing.iter().chain(written))
            .into_iter()
            .filter_map(|(_, text)| containers::reference(text))
            .collect();
        let mut downloads = 2 * named.len();
        let reader = Arc::new(WriteContainers::default());
        let mut environment = environment.clone();
        environment.containers =
            Some(Arc::clone(&reader) as Arc<dyn ContainerReader + Send + Sync>);
        // A formula following a relationship by a match field the write
        // computes asks for it to be read with the computed key (#1035): it
        // is, and the record is computed again, a few times at most (a key
        // computed from what another such read gives).
        let needs = Arc::new(om_data::rules::RelatedNeeds::default());
        environment.related_needs = Some(Arc::clone(&needs));
        let mut keyed: BTreeMap<(u64, u64), Vec<String>> = BTreeMap::new();
        let mut rounds = KEY_ROUNDS;
        loop {
            // A lookup whose calculation read the objects of a layout listed
            // from the catalog's index only (#1019): its member is read, and the
            // rules are planned again over the grown schema. Record rules
            // themselves call no Design function, nor Evaluate.
            let mut replanned: Option<RecordPlan> = None;
            // A preview without the window's unsaved records reads nothing of
            // another record (preview.rs).
            if read {
                loop {
                    let plan = replanned.as_ref().unwrap_or(plan);
                    let (lookups, layouts) = self.read_lookups(
                        plan,
                        (mode, entry),
                        existing,
                        changes,
                        environment.held.as_deref(),
                        (&keyed, which),
                    )?;
                    if layouts.is_empty() {
                        environment.lookups = lookups;
                        break;
                    }
                    replanned = Some(self.replan_for_layouts(plan.table(), layouts)?);
                }
            }
            // Serials not taken yet: a held record's on creation were taken
            // when it was created (`hold_new`).
            if reserve {
                let missing = plan
                    .serials_for(mode, entry)
                    .into_iter()
                    .filter(|serial| !environment.serials.contains_key(&serial.field));
                let reserved = self.reserve_serials(plan.table(), missing)?;
                environment.serials.extend(reserved);
            }
            let prepared = loop {
                let plan = replanned.as_ref().unwrap_or(plan);
                let prepared = compute(plan, &environment);
                let Some(need) = reader.take_need() else {
                    break prepared;
                };
                if downloads == 0 {
                    // A calculation naming members the record does not.
                    return Err(DataError::with_code(
                        om_data::rules::error::UNAVAILABLE,
                        "A calculation reads container data the record does not hold. \
                     No values were changed.",
                    ));
                }
                downloads -= 1;
                self.read_container_need(&reader, need)?;
            };
            let more = needs.take();
            if more.is_empty() || rounds == 0 {
                return prepared;
            }
            rounds -= 1;
            keyed.extend(more);
        }
    }

    /// What the write's lookups copy ([`RecordPlan::read_lookups`]), read
    /// from the server through the calculations' own source: the related
    /// table's value index narrows its records, the scan decides. A write
    /// in a transaction reads the records it holds over the stored ones
    /// (`held`, [`om_data::rules::HeldSource`]).
    fn read_lookups(
        &mut self,
        plan: &RecordPlan,
        (mode, entry): (om_data::rules::WriteMode, Entry),
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        held: Option<&om_data::rules::HeldRecords>,
        (keyed, written): (&BTreeMap<(u64, u64), Vec<String>>, om_data::rules::Written),
    ) -> Result<(om_data::rules::Lookups, BTreeSet<u32>), DataError> {
        if !plan.has_lookups() {
            return Ok(Default::default());
        }
        // As an owning file the opened file writes, its formulas read the
        // opened file's tables through that file's session (#187).
        let caller = self.caller.as_mut();
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DataError::new("No file is open."))?;
        let today = om_data::host::session::today(0);
        let source = crate::calc::NativeSource::new(session, plan.schema(), today)
            .with_fonts(&self.calc.fonts);
        // Planned over the provider's own schema, a related table is read
        // on its file's connection, as the window's reads are (#1139). An
        // owning file's routed plan reads the opened file's tables and its
        // other related files' through the sessions lent below (#187,
        // #1160).
        let source = match self.routed_plan {
            None => source.with_external(&self.external),
            Some(_) => source,
        };
        let root = caller.map(
            |crate::related_owner::CallerReads {
                 session,
                 schema,
                 tables,
                 globals,
                 pending,
                 others,
                 ..
             }| {
                let others: Vec<_> = others
                    .iter_mut()
                    .map(|(_, other)| {
                        (
                            crate::calc::NativeSource::new(
                                &mut other.session,
                                &other.schema,
                                today,
                            ),
                            &other.tables,
                            other.pending.as_deref().map(|held| (held, &*other.schema)),
                        )
                    })
                    .collect();
                (
                    crate::calc::NativeSource::new(session, schema, today)
                        .with_session_globals(globals.clone()),
                    &*tables,
                    pending.as_deref().map(|pending| (pending, &**schema)),
                    others,
                )
            },
        );
        // The opened file's window's unsaved records over its stored ones,
        // for a preview (preview.rs).
        let root_pending = root.as_ref().and_then(|(root, _, pending, _)| {
            pending.map(|(held, schema)| om_data::rules::HeldSource::new(root, held, schema))
        });
        let peer_pending: Vec<_> = root.as_ref().map_or_else(Vec::new, |(_, _, _, others)| {
            others
                .iter()
                .map(|(source, _, pending)| {
                    pending
                        .map(|(held, schema)| om_data::rules::HeldSource::new(source, held, schema))
                })
                .collect()
        });
        let overlay =
            held.map(|held| om_data::rules::HeldSource::new(&source, held, plan.schema()));
        let own: &dyn om_data::host::CalcSource = match &overlay {
            Some(overlay) => overlay,
            None => &source,
        };
        let read = match &root {
            Some((root, tables, _, others)) => {
                let root: &dyn om_data::host::CalcSource = match &root_pending {
                    Some(pending) => pending,
                    None => root,
                };
                let others: Vec<(&dyn om_data::host::CalcSource, &BTreeMap<u64, u64>)> = others
                    .iter()
                    .zip(&peer_pending)
                    .map(|((source, tables, _), pending)| {
                        let source: &dyn om_data::host::CalcSource = match pending {
                            Some(pending) => pending,
                            None => source,
                        };
                        (source, *tables)
                    })
                    .collect();
                let mapped = crate::related_owner::CallerSource {
                    own,
                    root,
                    tables,
                    others: &others,
                };
                plan.read_lookups_with(mode, existing, changes, entry, &mapped, (keyed, written))
            }
            None => plan.read_lookups_with(mode, existing, changes, entry, own, (keyed, written)),
        };
        // A download that failed is the write's refusal, not the lookup's.
        let root_failure = root.as_ref().and_then(|(root, _, _, others)| {
            root.take_failure()
                .or_else(|| others.iter().find_map(|(other, _, _)| other.take_failure()))
        });
        if let Some(failure) = source.take_failure().or(root_failure) {
            return Err(failure);
        }
        // Only layouts: the plan's schema may be an owning file's own
        // (#187), whose occurrences are not the opened file's to open.
        Ok((read?, source.take_needs().layouts))
    }

    /// The rules of base table `table` planned again once the members of
    /// `layouts`, which a write's calculations read, are read (#1019).
    /// Refused when none of them changed what the schema holds, so a write
    /// never runs on a layout it reads as having no objects.
    fn replan_for_layouts(
        &mut self,
        table: u64,
        layouts: BTreeSet<u32>,
    ) -> Result<RecordPlan, DataError> {
        if !self.read_layouts(layouts)? {
            return Err(DataError::with_code(
                om_data::rules::error::UNAVAILABLE,
                "A calculation reads a layout that could not be read. No values were changed.",
            ));
        }
        Ok(self.table_plan(table)?.1)
    }

    /// Download what a run of a write's rules lacked into `reader`.
    fn read_container_need(
        &mut self,
        reader: &WriteContainers,
        need: CalcNeed,
    ) -> Result<(), DataError> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DataError::new("No file is open."))?;
        let cache = self.calc.library.get_mut();
        match need {
            CalcNeed::Member { table, member } => {
                let info = cache.member(session, table, &member)?;
                reader.put_member((table, member), info);
            }
            CalcNeed::MemberData { table, member } => {
                let data = cache.data(session, table, &member)?;
                reader.put_data((table, member), data);
            }
            // The reader asks for nothing else.
            _ => {}
        }
        Ok(())
    }

    /// [`om_data::DataProvider::container_reference`].
    pub(crate) fn store_container(
        &mut self,
        layout_id: u32,
        field: &str,
        value: &Container,
    ) -> Result<Option<String>, DataError> {
        self.require_open()?;
        let (table, _) = self.record_plan(layout_id)?;
        self.store_table_container(&table, Some(field.to_owned()), value, Some(layout_id))
    }

    /// [`Self::store_container`] into field `field` (by name) of `table`,
    /// the layout's or a related record's base table.
    pub(crate) fn store_table_container(
        &mut self,
        table: &Table,
        field: Option<String>,
        value: &Container,
        entry_layout: Option<u32>,
    ) -> Result<Option<String>, DataError> {
        let field = field.unwrap_or_default();
        let field = field.as_str();
        let (table, field) = match table.field_by_name(field) {
            Some(f) if f.data_type == DataType::Container => {
                if f.kind != om_hbam::FieldKind::Normal
                    || !f
                        .decoded_options()
                        .is_some_and(|o| o.options & 0x1_0000 == 0)
                {
                    return Err(DataError::with_code(
                        om_data::rules::error::FIELD_LOCKED,
                        format!("Field {field} cannot be edited."),
                    ));
                }
                (table.id, f.id)
            }
            _ => {
                return Err(DataError::with_code(
                    om_data::rules::error::FIELD_MISSING,
                    format!("{field} is not a container field."),
                ))
            }
        };
        let checked_copy = value.library.is_some()
            && (self.prepared_attachment_group()
                || self.single_stored_copy_candidate(table, field, value));
        if checked_copy {
            self.prepare_stored_copy(
                entry_layout.ok_or_else(|| {
                    DataError::with_code(3, "Stored copy requires its accepted Root layout.")
                })?,
                table,
                field,
                value,
            )?;
        } else {
            self.check_asset_acquisition(table, Some(field))?;
        }
        let uploaded_input = value.library.is_none();
        if !uploaded_input && !checked_copy {
            self.leave_single_root_recovery()?;
        }
        let mut value = value.clone();
        if let Some(r) = value.library.clone().filter(|_| value.streams.is_empty()) {
            // Fetch on the owning file's session, then verify the original
            // identity. A stale id must never count or copy its replacement.
            let schema = self.calc_schema_cached()?;
            let same_library = r.table == table
                && r.owner
                    == Some(crate::library::file_key(
                        self.session.as_ref().expect("open session"),
                    ));
            let read = |session: &mut om_session::Session, wire| {
                let mut cache = self.calc.library.borrow_mut();
                cache.refresh();
                let Some(info) = cache.member(session, wire, &r.member)? else {
                    return Err(om_data::DataError::with_code(
                        1225,
                        "The source container no longer exists.",
                    ));
                };
                if info.owner != r.owner || info.md5 != r.md5 {
                    return Err(om_data::DataError::with_code(
                        1225,
                        "The source container was replaced or its file was closed.",
                    ));
                }
                if same_library {
                    Ok(None)
                } else {
                    cache
                        .data_as(session, wire, &r.member, r.table)?
                        .map(Some)
                        .ok_or_else(|| {
                            DataError::with_code(1225, "The source container data is unavailable.")
                        })
                }
            };
            let stored = if let Some(route) = self.external.route(r.table) {
                self.external.with_session(route, read)?
            } else if schema.table_context(r.table).is_some() {
                return Err(om_data::DataError::with_code(
                    1225,
                    "The source file is not open.",
                ));
            } else {
                read(self.session.as_mut().expect("open session"), r.table)?
            };
            if same_library {
                if checked_copy {
                    self.begin_stored_copy(table, field, &value)?;
                }
                let count = self
                    .session_mut()?
                    .adjust_reference_count(member_path(table, &r.member), 1)
                    .map_err(|e| failure("count a container reference", e))?;
                if count != COUNT_FAILED {
                    self.note_container_taken(table, &r.member, 1);
                    if checked_copy {
                        self.acknowledge_stored_copy(table, &r.member, count)?;
                    }
                    return Ok(Some(containers::reference_text(&r.member)));
                }
                return Err(DataError::with_code(
                    1225,
                    "The source container no longer exists.",
                ));
            }
            let stored = stored.expect("another library's data");
            if stored.external && !stored.intact {
                return Err(DataError::with_code(
                    1225,
                    "The external container data is unavailable.",
                ));
            }
            value = stored.data.clone();
        }
        let Some((keys, _)) = containers::tree(&value) else {
            return Ok(None);
        };
        let payload = OwnedNode::list(Vec::new(), keys)
            .to_wire()
            .ok_or_else(|| DataError::new("The container data is too large for a slot."))?;
        if let Some(t) = &mut self.transaction {
            t.upload_attempted = true;
        }
        if uploaded_input {
            self.begin_single_upload(table, field, &payload);
        }
        let reply = self
            .session_mut()?
            .upload_binary(&key_bytes(table), &key_bytes(field), payload)
            .map_err(|e| failure("store container data", e))?;
        match (reply.status, reply.member.as_slice()) {
            (0 | LIBRARY_OBJECT_EXISTS, [member]) => {
                self.calc.library.get_mut().refresh();
                self.note_container_taken(table, member, 1);
                if uploaded_input {
                    self.acknowledge_root_acquisition(table, member);
                }
                Ok(Some(containers::reference_text(member)))
            }
            (status, _) => Err(DataError::with_code(
                i32::from(status),
                format!(
                    "The host did not store the container data (status {status}). {}",
                    reply.error
                ),
            )),
        }
    }

    /// [`om_data::DataProvider::adjust_container`].
    pub(crate) fn adjust_container_reference(
        &mut self,
        layout_id: u32,
        reference: &str,
        delta: i32,
    ) -> Result<(), DataError> {
        self.require_open()?;
        let layout = self.decoded_layout(layout_id)?;
        let table = self.table_for_layout(&layout)?.id;
        self.adjust_table_container(table, reference, delta)
    }

    /// [`Self::adjust_container_reference`] in base table `table`'s library.
    pub(crate) fn adjust_table_container(
        &mut self,
        table: u64,
        reference: &str,
        delta: i32,
    ) -> Result<(), DataError> {
        let Some(member) = containers::reference(reference) else {
            return Ok(());
        };
        if delta < 0 {
            self.begin_asset_release(table, &member, delta.saturating_neg())?;
            // A release whose reply is lost (a timeout) may still have been
            // applied: it leaves the references this window or transaction
            // would release or revert before it is sent, as a direct write's
            // acquisitions do (`store_pending_assets`), so that no later
            // join or revert releases it again. One that was not applied
            // leaves the count one high: the member is kept, never lost.
            self.count_container(table, &member, delta);
        } else if delta > 0 {
            self.check_asset_acquisition(table, None)?;
        }
        let count = self
            .session_mut()?
            .adjust_reference_count(member_path(table, &member), delta)
            .map_err(|e| failure("count a container reference", e))?;
        if count == COUNT_FAILED {
            if delta > 0 {
                return Err(DataError::with_code(
                    om_data::rules::error::UNAVAILABLE,
                    "The container member to copy no longer exists.",
                ));
            }
            return Err(DataError::with_code(3, "Unresolved: the container acquisition is missing. Drafts and ownership evidence are kept."));
        }
        if count != COUNT_FAILED {
            self.note_container_taken(table, &member, delta);
        }
        self.calc.library.get_mut().refresh();
        Ok(())
    }

    /// A reference to `member` of `table`'s library was taken (`delta` > 0)
    /// or released, as its library answered. A release has left the counts
    /// before it was sent ([`Self::adjust_table_container`]).
    fn note_container_taken(&mut self, table: u64, member: &[u8], delta: i32) {
        if delta > 0 {
            self.note_asset_acquisition(table, member);
            if let Some(asset) = self.acquired_asset(table, member, delta) {
                self.add_asset_receipt(table, asset);
            }
            self.count_container(table, member, delta);
        } else if delta < 0 {
            self.acknowledge_asset_release(table, member, delta.saturating_neg());
        }
    }

    /// The references to `member` of `table`'s library the window's
    /// records or its transaction hold, which their commit keeps and their
    /// revert releases, changed by `delta`.
    fn count_container(&mut self, table: u64, member: &[u8], delta: i32) {
        let counts = match self.transaction.as_mut() {
            Some(t) => &mut t.containers,
            None => self.pending_assets.entry(self.window).or_default(),
        };
        let count = counts.entry((table, member.to_vec())).or_default();
        *count = (*count + delta).max(0);
        if delta < 0 {
            if let Some(t) = &mut self.transaction {
                if let Some(count) = t.root_acquisitions.get_mut(&(table, member.to_vec())) {
                    *count = (*count + delta).max(0);
                }
            }
        }
    }

    /// Release one reference to each of `members` of `table`'s library,
    /// after a commit that no longer holds them (5.1, 7.1-7.3).
    pub(crate) fn release_members(&mut self, table: u64, members: Vec<Vec<u8>>) {
        let Ok(session) = self.session_mut() else {
            return;
        };
        for member in members {
            match session.adjust_reference_count(member_path(table, &member), -1) {
                Ok(count) if count != COUNT_FAILED => {}
                Ok(_) => warn!("released container member {member:02x?} was not in its library"),
                Err(e) => warn!("releasing container member {member:02x?} failed: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn need(member: u8) -> CalcNeed {
        CalcNeed::Member {
            table: 0x8001,
            member: vec![member],
        }
    }

    #[test]
    fn a_run_notes_the_first_read_it_lacks_and_counts_every_one() {
        fn shared<T: Send + Sync>(_: &T) {}
        let reader = WriteContainers::default();
        shared(&reader);
        let lacked = reader.member(0x8001, &[1]).unwrap_err();
        assert_eq!(lacked.code(), 1224);
        // Read again, and another member read after it: pending, not noted.
        assert!(reader.member(0x8001, &[1]).is_err());
        assert!(reader.member_data(0x8001, &[2]).is_err());
        assert_eq!(reader.pending(), Pending { reads: 3, needs: 1 });
        assert_eq!(reader.take_need(), Some(need(1)));
        assert_eq!(reader.take_need(), None);

        // What was downloaded answers every later run, a member the
        // library lacks included.
        let info = Arc::new(MemberInfo {
            owner: None,
            name: "photo2.png".into(),
            md5: [7; 16],
            path_type: String::new(),
        });
        reader.put_member((0x8001, vec![1]), Some(Arc::clone(&info)));
        reader.put_member((0x8001, vec![3]), None);
        assert_eq!(reader.member(0x8001, &[1]).unwrap(), Some(info));
        assert_eq!(reader.member(0x8001, &[3]).unwrap(), None);
        assert_eq!(reader.take_need(), None);
        // Counts only grow.
        assert!(reader.member_data(0x8001, &[1]).is_err());
        assert_eq!(reader.pending(), Pending { reads: 4, needs: 2 });
        assert_eq!(
            reader.take_need(),
            Some(CalcNeed::MemberData {
                table: 0x8001,
                member: vec![1],
            })
        );
    }
}
