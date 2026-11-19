//! Before-work row participation is independent of acquiring an attachment.
use super::*;

#[derive(Clone)]
pub(crate) enum RowReceipt {
    Creating {
        table: u64,
        input: BTreeMap<u64, Option<String>>,
        reservation: Option<u64>,
        serials: BTreeMap<u64, String>,
        preview: Option<om_data::PreparedId>,
        preview_stamp: Option<preview::Stamp>,
    },
    New {
        table: u64,
        record: u64,
    },
    Existing(ExistingRoot),
}

#[derive(Clone)]
pub(super) enum RowIntent {
    New,
    Existing { record: u64, version: u64 },
}

impl RowReceipt {
    pub(super) fn creating(table: u64) -> Self {
        Self::Creating {
            table,
            input: BTreeMap::new(),
            reservation: None,
            serials: BTreeMap::new(),
            preview: None,
            preview_stamp: None,
        }
    }
    pub(crate) fn key(&self) -> Option<(u64, u64)> {
        match self {
            Self::Creating { .. } => None,
            Self::New { table, record } => Some((*table, *record)),
            Self::Existing(e) => Some((e.table, e.record)),
        }
    }
    pub(crate) fn new_row(&self) -> bool {
        matches!(self, Self::New { .. })
    }
    fn agrees(&self, intent: &RowIntent, wire: u64) -> bool {
        match (self, intent) {
            (Self::New { table, .. }, RowIntent::New) => *table == wire,
            (Self::Existing(e), RowIntent::Existing { record, version }) => {
                (e.table, e.record, e.version) == (wire, *record, *version)
            }
            _ => false,
        }
    }
    fn covers(&self, records: &transaction::ScalarRecords) -> bool {
        self.key()
            .is_some_and(|(t, r)| records.single_table() == Some(t) && records.contains(t, r))
    }
}

/// A preparation names one actual owner row and the supported first repetition.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Acquisition {
    token: u64,
    table: u64,
    record: u64,
    field: u64,
    // The existing typed preparation and normal Insert support repetition 1.
    repetition: u32,
}

/// Membership is per logical row; acquisition/transaction ownership stays per file.
/// Tokens are assigned before any reservation and never inferred from a RID.
#[derive(Clone, Default)]
pub(crate) struct Rows {
    pub(crate) members: BTreeMap<u64, RowReceipt>,
    pub(crate) creating: Option<u64>,
    pub(super) acquisition: Option<Acquisition>,
}

impl Rows {
    pub(crate) fn covers(&self, records: &transaction::ScalarRecords) -> bool {
        let keys: Option<BTreeSet<_>> = self.members.values().map(RowReceipt::key).collect();
        keys.is_some_and(|keys| {
            keys.len() == self.members.len()
                && keys.len() == records.len()
                && keys.iter().all(|(t, r)| records.contains(*t, *r))
        })
    }
}

impl ScalarCarry {
    pub(super) fn mixed_membership(&self) -> bool {
        self.kind == CarryKind::PreparedMixedGroup
            && self.row.as_ref().is_some_and(|row| {
                row.covers(&self.root)
                    && match row {
                        RowReceipt::New { .. } => true,
                        RowReceipt::Existing(existing) => {
                            self.existing.as_ref().is_some_and(|original| {
                                original.table == existing.table
                                    && original.record == existing.record
                                    && original.version == existing.version
                                    && original.row == existing.row
                            })
                        }
                        RowReceipt::Creating { .. } => false,
                    }
            })
            && !self.owners.is_empty()
            && self.owners.len() <= external::MAX_FILES
            && self.intents.len() <= 4096
            && self.intents.iter().enumerate().all(|(i, intent)| {
                intent.token == Some(i as u64)
                    && intent.input.is_some()
                    && self.occurrences.contains(&intent.occurrence)
                    && self
                        .owners
                        .iter()
                        .filter(|o| {
                            intent.identity.as_ref() == Some(&o.identity)
                                && o.routes.contains(&(intent.table, intent.wire))
                                && o.rows.members.get(&(i as u64)).is_some_and(|r| {
                                    intent
                                        .row
                                        .as_ref()
                                        .is_some_and(|want| r.agrees(want, intent.wire))
                                })
                        })
                        .count()
                        == 1
            })
            && self.owners.iter().enumerate().all(|(i, o)| {
                o.rows.covers(&o.records)
                    && !o.rows.members.is_empty()
                    && o.rows.creating.is_none()
                    && o.rows.members.keys().all(|token| {
                        self.intents
                            .get(*token as usize)
                            .is_some_and(|intent| intent.identity.as_ref() == Some(&o.identity))
                    })
                    && self.owners[..i].iter().all(|p| p.identity != o.identity)
            })
    }
}

impl NativeProvider {
    pub(crate) fn retain_creation_input(
        &mut self,
        table: u64,
        cells: &BTreeMap<u64, Option<String>>,
        prepared: Option<om_data::PreparedId>,
    ) {
        if let Some(RowReceipt::Creating {
            table: own,
            input,
            preview,
            ..
        }) = self
            .transaction
            .as_mut()
            .and_then(|t| t.single_root_start.as_mut())
            .and_then(|s| {
                if s.rows.members.is_empty() {
                    s.row.as_mut()
                } else {
                    let token = s.rows.creating?;
                    s.rows.members.get_mut(&token)
                }
            })
        {
            if *own == table {
                *input = cells.clone();
                *preview = prepared;
            }
        }
    }

    // Only a validated owner-bound preview may supply already issued values.
    pub(crate) fn retain_creation_preview(&mut self, table: u64, reuse: &preview::Reuse) {
        if let Some(RowReceipt::Creating {
            table: own,
            serials,
            preview_stamp,
            ..
        }) = self
            .transaction
            .as_mut()
            .and_then(|t| t.single_root_start.as_mut())
            .and_then(|s| {
                if s.rows.members.is_empty() {
                    s.row.as_mut()
                } else {
                    let token = s.rows.creating?;
                    s.rows.members.get_mut(&token)
                }
            })
        {
            if *own == table {
                *serials = reuse.serials.clone();
                *preview_stamp = Some(reuse.stamp.clone());
            }
        }
    }

    pub(crate) fn retain_creation_reservation(&mut self, table: u64, record: u64) {
        if let Some(RowReceipt::Creating {
            table: own,
            reservation,
            ..
        }) = self
            .transaction
            .as_mut()
            .and_then(|t| t.single_root_start.as_mut())
            .and_then(|s| {
                if s.rows.members.is_empty() {
                    s.row.as_mut()
                } else {
                    let token = s.rows.creating?;
                    s.rows.members.get_mut(&token)
                }
            })
        {
            if *own == table {
                *reservation = Some(record);
            }
        }
    }

    pub(crate) fn retain_creation_serial(&mut self, table: u64, field: u64, value: &str) {
        if let Some(RowReceipt::Creating {
            table: own,
            serials,
            ..
        }) = self
            .transaction
            .as_mut()
            .and_then(|t| t.single_root_start.as_mut())
            .and_then(|s| {
                if s.rows.members.is_empty() {
                    s.row.as_mut()
                } else {
                    let token = s.rows.creating?;
                    s.rows.members.get_mut(&token)
                }
            })
        {
            if *own == table {
                serials.insert(field, value.to_owned());
            }
        }
    }

    pub(crate) fn recovery_row(&self) -> Option<RowReceipt> {
        self.transaction
            .as_ref()?
            .single_root_start
            .as_ref()?
            .row
            .clone()
    }

    pub(super) fn mixed_group_shape(&self, c: &ScalarCarry, retaining: bool) -> bool {
        c.mixed_membership()
            && self.prepared_joined_membership(c)
            && self.mixed_row_shape(
                false,
                retaining
                    && c.mark.until(om_data::DataProvider::submission_mark(self))
                        == Submitted::Nothing,
            )
            && self
                .scalar_root_records()
                .is_some_and(|r| r.same_records(&c.root))
            && c.owners.iter().all(|p| {
                self.external.route(p.table).is_some_and(|route| {
                    self.external
                        .with_owner(route, |owner, _| {
                            Ok(owner.mixed_row_shape(true, false)
                                && owner
                                    .scalar_records()
                                    .is_some_and(|r| r.same_records(&p.records)))
                        })
                        .unwrap_or(false)
                })
            })
    }

    fn input_fields(
        stamp: &Stamp,
        wire: u64,
        write: &om_data::ScriptWrite,
    ) -> Result<BTreeSet<u64>, DataError> {
        let table = stamp.catalog.table(wire).ok_or_else(unresolved)?;
        let mut fields = write
            .cells
            .keys()
            .map(|cell| {
                table
                    .field(*cell)
                    .or_else(|| table.field(*cell >> 8))
                    .map(|f| f.id)
                    .ok_or_else(unresolved)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        for name in write.fields.keys() {
            fields.insert(table.field_by_name(name).ok_or_else(unresolved)?.id);
        }
        Ok(fields)
    }

    // Admission precedes every join, lock and reservation. A declared failure
    // leaves the whole group incomplete, never an eligible earlier subset.
    pub(super) fn prepare_mixed_write(
        &mut self,
        write: &om_data::ScriptWrite,
        table: u64,
    ) -> Result<bool, DataError> {
        let Some(mut c) = self
            .scalar_carry
            .as_ref()
            .filter(|c| {
                c.kind == CarryKind::PreparedMixedGroup
                    || c.kind.single() && c.row.is_some() && self.external.route(table).is_some()
            })
            .cloned()
        else {
            return Ok(false);
        };
        if c.assets_detached {
            return Err(unresolved());
        }
        // A newly demanded table can extend an already accepted file's alias
        // map; every prior mapping and the original owning stamp must survive.
        if let Some(route) = self.external.route(table) {
            let identity = self.external.route_identity(route).ok_or_else(unresolved)?;
            if let Some(part) = c.owners.iter_mut().find(|p| p.identity == identity) {
                if !self
                    .external
                    .route(part.table)
                    .is_some_and(|original| original.same_file(route))
                {
                    return Err(unresolved());
                }
                let routes = self.external.route_tables(route);
                if !part.routes.iter().all(|r| routes.contains(r)) {
                    return Err(unresolved());
                }
                part.routes = routes;
                self.external.with_owner(route, |owner, _| {
                    if !part.stamp.admit_catalogs(owner) {
                        return Err(unresolved());
                    }
                    Ok(())
                })?;
            }
        }
        self.validate_carry_group(&c)?;
        if c.kind.single() {
            if !self.owner_transaction.alone() || !self.single_root_assets_match(&c) {
                return Err(unresolved());
            }
        } else if !self.mixed_group_shape(&c, true) {
            return Err(unresolved());
        }
        let Some(route) = self.external.route(table) else {
            let row = c.row.as_ref().ok_or_else(unresolved)?;
            if row.key()
                != write
                    .record
                    .and_then(|r| u64::try_from(r).ok())
                    .map(|r| (table, r))
            {
                return Err(unresolved());
            }
            let fields = Self::input_fields(&c.stamp, table, write)?;
            c.stamp.needs.as_mut().ok_or_else(unresolved)?.write(
                table,
                row.new_row(),
                fields,
                &[write.layout].into_iter().collect(),
            );
            if !c.stamp.proven() {
                return Err(unresolved());
            }
            self.scalar_carry = Some(c);
            return Ok(true);
        };
        let identity = self.external.route_identity(route).ok_or_else(unresolved)?;
        let part = c.owners.iter().position(|p| p.identity == identity);
        if let Some(i) = part {
            if !c.owners[i].routes.contains(&(table, route.table)) {
                return Err(unresolved());
            }
            if let Some(record) = write.record.and_then(|r| u64::try_from(r).ok()) {
                if let Some(row) = c.owners[i]
                    .rows
                    .members
                    .values()
                    .find(|r| r.key() == Some((route.table, record)))
                {
                    let new = row.new_row();
                    let fields = Self::input_fields(&c.owners[i].stamp, route.table, write)?;
                    let definition = c.owners[i]
                        .stamp
                        .catalog
                        .table(route.table)
                        .ok_or_else(unresolved)?;
                    for field in fields
                        .iter()
                        .filter_map(|id| definition.field(*id))
                        .filter(|f| f.data_type == om_hbam::DataType::Container)
                    {
                        let bound = c.owners[i].rows.acquisition.is_some_and(|a| {
                            a.field == field.id
                                && a.repetition == 1
                                && (a.table, a.record) == (route.table, record)
                                && c.owners[i]
                                    .rows
                                    .members
                                    .get(&a.token)
                                    .and_then(RowReceipt::key)
                                    == Some((route.table, record))
                        });
                        let same = |cell, value| {
                            c.owners[i]
                                .records
                                .same_cell(route.table, record, cell, value)
                        };
                        if write
                            .fields
                            .get(&field.name)
                            .is_some_and(|value| !bound && !same(field.id, value))
                        {
                            return Err(unresolved());
                        }
                        // Another row's binding, or the first repetition's binding,
                        // permits only unchanged previously accepted other cells.
                        for (cell, value) in &write.cells {
                            let actual = definition
                                .field(*cell)
                                .or_else(|| definition.field(*cell >> 8));
                            if actual.is_some_and(|f| f.id == field.id)
                                && (!bound || *cell != field.id)
                                && !same(*cell, value)
                            {
                                return Err(unresolved());
                            }
                        }
                    }
                    c.owners[i]
                        .stamp
                        .needs
                        .as_mut()
                        .ok_or_else(unresolved)?
                        .write(route.table, new, fields, &BTreeSet::new());
                    if !c.owners[i].stamp.proven() {
                        return Err(unresolved());
                    }
                    self.scalar_carry = Some(c);
                    return Ok(true);
                }
            }
        }
        if c.intents.len() >= 4096 || (part.is_none() && c.owners.len() >= external::MAX_FILES) {
            return Err(unresolved());
        }
        let occurrence = self
            .calc
            .schema
            .as_ref()
            .ok_or_else(unresolved)?
            .occurrences()
            .find(|o| {
                self.calc
                    .schema
                    .as_ref()
                    .and_then(|s| s.table_of(o.id))
                    .is_some_and(|t| t.id == table)
            })
            .map(|o| o.id)
            .ok_or_else(unresolved)?;
        let intent = match write.record {
            Some(id) => {
                let record = u64::try_from(id).map_err(|_| unresolved())?;
                let version = self
                    .pinned
                    .get(&(self.window, table, record))
                    .copied()
                    .flatten()
                    .ok_or_else(unresolved)?;
                RowIntent::Existing { record, version }
            }
            None => RowIntent::New,
        };
        let token = c.intents.len() as u64;
        let (record, version) = match intent {
            RowIntent::New => (0, 0),
            RowIntent::Existing { record, version } => (record, version),
        };
        c.kind = CarryKind::PreparedMixedGroup;
        c.complete = false;
        c.intents.push(OwnerIntent {
            token: Some(token),
            table,
            wire: route.table,
            record,
            version,
            occurrence,
            identity: Some(identity.clone()),
            row: Some(intent.clone()),
            input: Some(write.clone()),
        });
        if !c.occurrences.contains(&occurrence) {
            c.occurrences.push(occurrence);
        }
        self.scalar_carry = Some(c.clone());
        self.join_owner_transaction(table, route)?;
        let preparation = self.external.with_owner(route, |owner, wire| {
            owner.calc_schema_cached()?;
            let mut stamp = part
                .map(|i| c.owners[i].stamp.clone())
                .unwrap_or_else(|| Stamp::read(owner));
            let fields = Self::input_fields(&stamp, wire, write)?;
            if fields.iter().any(|id| {
                stamp
                    .catalog
                    .table(wire)
                    .and_then(|t| t.field(*id))
                    .is_some_and(|f| f.data_type == om_hbam::DataType::Container)
            }) {
                // A new logical row must first be admitted without a borrowed
                // container reference. Its explicit target preparation follows.
                return Err(unresolved());
            }
            stamp.needs.as_mut().ok_or_else(unresolved)?.write(
                wire,
                matches!(intent, RowIntent::New),
                fields.clone(),
                &BTreeSet::new(),
            );
            if !stamp.proven() || !stamp.explicit_identity() {
                return Err(unresolved());
            }
            let existing = match intent {
                RowIntent::New => None,
                RowIntent::Existing { record, version } => {
                    let row = owner.read_record(wire, record)?.ok_or_else(unresolved)?;
                    if records::mod_count(&records::stored_record(record, &row))? != version {
                        return Err(unresolved());
                    }
                    Some(ExistingRoot {
                        table: wire,
                        record,
                        version,
                        row,
                        library: None,
                        observed: Vec::new(),
                    })
                }
            };
            let row = existing
                .clone()
                .map_or(RowReceipt::creating(wire), RowReceipt::Existing);
            let mut rows = part.map(|i| c.owners[i].rows.clone()).unwrap_or_default();
            if rows.creating.is_some() || rows.members.insert(token, row).is_some() {
                return Err(unresolved());
            }
            rows.creating = matches!(intent, RowIntent::New).then_some(token);
            let start = RootStart {
                rows: rows.clone(),
                participant: true,
                upload: None,
                mark: c.mark,
                stamp: stamp.clone(),
                identity: identity.clone(),
                existing,
                row: None,
            };
            if let Some(i) = part {
                c.owners[i].rows = rows;
                c.owners[i].stamp = stamp;
            } else {
                c.owners.push(Owner {
                    rows,
                    table,
                    routes: self.external.route_tables(route),
                    identity,
                    stamp,
                    records: transaction::ScalarRecords::empty(),
                    assets: Vec::new(),
                    existing: None,
                    pending_acquisition: None,
                    row: None,
                });
            }
            // Keep originals and declared membership even if the first lock's
            // ACK is lost. No incomplete prefix can become recoverable.
            owner.prepare_mixed_part(start, fields)
        });
        self.scalar_carry = Some(c);
        preparation?;
        Ok(true)
    }

    pub(super) fn collect_mixed_part(&mut self, table: u64) -> bool {
        if !self
            .scalar_carry
            .as_ref()
            .is_some_and(|c| c.kind == CarryKind::PreparedMixedGroup)
        {
            return false;
        }
        let Some(mut c) = self.scalar_carry.take() else {
            return true;
        };
        let mut collected = false;
        if let Some(p) = c
            .owners
            .iter_mut()
            .find(|p| p.routes.iter().any(|(client, _)| *client == table))
        {
            let evidence = self.external.route(table).and_then(|route| {
                self.external
                    .with_owner(route, |owner, _| {
                        let pending = owner
                            .transaction
                            .as_ref()
                            .and_then(|t| t.single_root_start.as_ref())
                            .and_then(|s| s.upload.clone());
                        Ok((
                            owner
                                .transaction
                                .as_ref()
                                .and_then(|t| t.single_root_start.as_ref())
                                .map(|s| s.rows.clone())
                                .unwrap_or_default(),
                            pending,
                            owner.scalar_records(),
                            owner.carry_assets(),
                            owner.scalar_footprint(),
                        ))
                    })
                    .ok()
            });
            if let Some((rows, pending, records, assets, needs)) = evidence {
                p.rows = rows;
                collected = p.collect_upload(pending, records, assets);
                if let (Some(original), Some(needs)) = (&mut p.stamp.needs, needs) {
                    original.extend(needs);
                } else {
                    p.stamp.needs = None;
                }
            }
        }
        c.complete = collected
            && c.mixed_membership()
            && self.prepared_joined_membership(&c)
            && c.stamp.proven()
            && c.pending_acquisition.is_none()
            && c.owners.iter().all(|p| {
                p.pending_acquisition.is_none()
                    && p.stamp.proven()
                    && p.assets.iter().all(asset_recovery::Asset::ownership_known)
            });
        self.scalar_carry = Some(c);
        true
    }

    // Existing typed preparation supplies an accepted RID/version; new targets
    // must name an acknowledged Held reservation, never a guessed creation row.
    pub(super) fn prepare_mixed_container_target(
        &mut self,
        table: u64,
        record: i64,
        version: Option<u64>,
        field: u64,
        root: Option<om_data::ContainerEditBaseline>,
    ) -> Result<Option<bool>, DataError> {
        let Some(mut c) = self
            .scalar_carry
            .as_ref()
            .filter(|c| c.kind == CarryKind::PreparedMixedGroup)
            .cloned()
        else {
            return Ok(None);
        };
        let route = self.external.route(table).ok_or_else(unresolved)?;
        let target = u64::try_from(record).map_err(|_| unresolved())?;
        if !c
            .owners
            .iter()
            .filter(|p| p.routes.contains(&(table, route.table)))
            .flat_map(|p| p.rows.members.values())
            .any(|r| r.key() == Some((route.table, target)))
        {
            let version = version.ok_or_else(unresolved)?;
            let layout = root.ok_or_else(unresolved)?.layout;
            if self
                .pinned
                .get(&(self.window, table, target))
                .is_some_and(|v| *v != Some(version))
            {
                return Err(unresolved());
            }
            if !om_data::DataProvider::retain_related_record(self, table, record, version) {
                return Err(unresolved());
            }
            om_data::DataProvider::write_record(
                self,
                &om_data::ScriptWrite {
                    layout,
                    table: Some(table),
                    record: Some(record),
                    ..Default::default()
                },
            )?;
            c = self.scalar_carry.clone().ok_or_else(unresolved)?;
        }
        self.validate_carry_group(&c)?;
        let record = target;
        let part = c
            .owners
            .iter_mut()
            .find(|p| p.routes.contains(&(table, route.table)))
            .ok_or_else(unresolved)?;
        let token = *part
            .rows
            .members
            .iter()
            .find(|(_, r)| r.key() == Some((route.table, record)))
            .map(|(k, _)| k)
            .ok_or_else(unresolved)?;
        let row = part.rows.members.get_mut(&token).ok_or_else(unresolved)?;
        let new = row.new_row();
        if let RowReceipt::Existing(original) = row {
            if version != Some(original.version) {
                return Err(unresolved());
            }
            let prepared = self.external.with_owner(route, |owner, wire| {
                let (definition, _) = owner.table_plan(wire)?;
                owner
                    .existing_attachment_start_at(
                        &definition,
                        ExistingAttachment {
                            record,
                            version: original.version,
                            field: Some(field),
                        },
                        None,
                        c.mark,
                        part.identity.clone(),
                    )?
                    .ok_or_else(unresolved)
            })?;
            let observed = prepared.existing.ok_or_else(unresolved)?;
            if observed.row != original.row {
                return Err(unresolved());
            }
            // Original baseline is never replaced by a later accepted row.
            original.library = observed.library;
            original.observed = observed.observed;
        }
        part.stamp.needs.as_mut().ok_or_else(unresolved)?.write(
            route.table,
            new,
            [field].into_iter().collect(),
            &BTreeSet::new(),
        );
        if !part.stamp.proven() || !part.stamp.library(route.table, true) {
            return Err(unresolved());
        }
        part.rows.acquisition = Some(Acquisition {
            token,
            table: route.table,
            record,
            field,
            repetition: 1,
        });
        let rows = part.rows.clone();
        self.external.with_owner(route, |owner, _| {
            owner
                .transaction
                .as_mut()
                .and_then(|t| t.single_root_start.as_mut())
                .ok_or_else(unresolved)?
                .rows = rows;
            Ok(())
        })?;
        self.scalar_carry = Some(c);
        Ok(Some(true))
    }

    pub(super) fn extend_mixed_acquisition(
        &mut self,
        table: u64,
        field: Option<u64>,
    ) -> Result<(), DataError> {
        let Some(c) = self
            .scalar_carry
            .as_ref()
            .filter(|c| c.kind == CarryKind::PreparedMixedGroup)
        else {
            return Ok(());
        };
        let field = field.ok_or_else(unresolved)?;
        if let Some(route) = self.external.route(table) {
            let p = c
                .owners
                .iter()
                .find(|p| p.routes.contains(&(table, route.table)))
                .ok_or_else(unresolved)?;
            let a = p.rows.acquisition.ok_or_else(unresolved)?;
            if a.field != field
                || a.repetition != 1
                || a.table != route.table
                || p.rows.members.get(&a.token).and_then(RowReceipt::key)
                    != Some((a.table, a.record))
            {
                return Err(unresolved());
            }
            if !p.stamp.proven() || !p.stamp.library(route.table, true) {
                return Err(unresolved());
            }
        } else {
            let row = c.row.as_ref().ok_or_else(unresolved)?;
            let (wire, _) = row.key().ok_or_else(unresolved)?;
            if !c.stamp.proven()
                || !c.stamp.library(wire, true)
                || c.stamp
                    .needs
                    .as_ref()
                    .and_then(|n| n.tables.get(&wire))
                    .is_none_or(|n| !n.fields.contains(&field))
            {
                return Err(unresolved());
            }
        }
        Ok(())
    }

    pub(super) fn restore_mixed_metadata(&mut self, p: &Owner) -> Result<(), DataError> {
        let route = self.external.route(p.table).ok_or_else(unresolved)?;
        for row in p.rows.members.values() {
            let (wire, record) = row.key().ok_or_else(unresolved)?;
            let client = self
                .external
                .client_table(route, wire)
                .ok_or_else(unresolved)?;
            self.restored_mixed_record(client, wire, record, row.new_row());
        }
        Ok(())
    }
}
