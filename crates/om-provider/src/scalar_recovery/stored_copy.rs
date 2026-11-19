//! Same-table stored copies prepare an existing Root or join its proved group.
//! A pending op87 is evidence only; ownership starts with its positive ACK.
use super::*;

#[derive(Clone)]
pub(super) struct CopyProof {
    pub(super) source: om_data::calc::value::ContainerSource,
    pub(super) destination: u64,
    pub(super) member: Vec<u8>,
    pub(super) fingerprint: asset_recovery::Fingerprint,
    pub(super) before: u32,
}

impl NativeProvider {
    pub(crate) fn prepare_stored_copy_entry(
        &mut self,
        baseline: om_data::ContainerEditBaseline,
        name: &str,
        value: &om_data::calc::Container,
    ) -> Result<om_data::EditPreparation, DataError> {
        use om_data::EditPreparation::{Prepared, Unavailable};
        self.validate_asset_change()?;
        let Some(library) = &value.library else {
            return Ok(Unavailable);
        };
        let Some(source) = library.source else {
            return Ok(Unavailable);
        };
        let Some(version) = baseline.version else {
            return Ok(Unavailable);
        };
        let Ok((table, _)) = self.record_plan(baseline.layout) else {
            return Ok(Unavailable);
        };
        let Some(destination) = table.field_by_name(name) else {
            return Ok(Unavailable);
        };
        if self.external.route(table.id).is_some()
            || source.table != table.id
            || library.table != table.id
            || Some(source.record) != u64::try_from(baseline.record).ok()
            || source.repetition != 1
            || source.field == destination.id
            || !value.streams.is_empty()
            || self
                .session
                .as_ref()
                .is_none_or(|s| library.owner != Some(crate::library::file_key(s)))
            || source.version < version
        {
            return Ok(Unavailable);
        }
        if source.version != version {
            return Err(DataError::with_code(
                306,
                "The accepted destination changed before stored-copy entry.",
            ));
        }
        let field = destination.id;
        if !self.prepare_existing_attachment(
            baseline.layout,
            baseline.record,
            Some(version),
            name,
        )? {
            return Ok(Unavailable);
        }
        if self.scalar_carry.is_none() && !self.ensure_single_root_receipt(table.id, field) {
            return Err(unresolved());
        }
        self.prepare_stored_copy(baseline.layout, table.id, field, value)?;
        Ok(Prepared)
    }

    pub(crate) fn single_stored_copy_candidate(
        &self,
        table: u64,
        destination: u64,
        value: &om_data::calc::Container,
    ) -> bool {
        let Some(c) = self
            .scalar_carry
            .as_ref()
            .filter(|c| c.kind == CarryKind::SingleExistingRoot)
        else {
            return false;
        };
        let Some(original) = &c.existing else {
            return false;
        };
        value.library.as_ref().is_some_and(|library| {
            library.table == table
                && self
                    .session
                    .as_ref()
                    .is_some_and(|s| library.owner == Some(crate::library::file_key(s)))
                && library.source.is_some_and(|source| {
                    source.table == original.table
                        && source.table == table
                        && source.record == original.record
                        && source.version == original.version
                        && source.field != destination
                        && source.repetition == 1
                })
        }) && value.streams.is_empty()
    }

    pub(crate) fn prepare_stored_copy(
        &mut self,
        layout: u32,
        table: u64,
        destination: u64,
        value: &om_data::calc::Container,
    ) -> Result<(), DataError> {
        let source = value
            .library
            .as_ref()
            .and_then(|library| library.source)
            .ok_or_else(unresolved)?;
        let member = value.library.as_ref().ok_or_else(unresolved)?;
        let carry = self.scalar_carry.clone().ok_or_else(unresolved)?;
        let original = carry.existing.as_ref().ok_or_else(unresolved)?;
        let single = carry.kind == CarryKind::SingleExistingRoot && carry.owners.is_empty();
        let group = carry.kind.group()
            && carry.owners.len() == 1
            && !carry.assets.is_empty()
            && !carry.owners.iter().any(|p| {
                p.assets.is_empty()
                    || !(if p.rows.members.is_empty() {
                        p.existing.is_some()
                    } else {
                        p.rows.members.len() == 1
                            && p.rows
                                .members
                                .values()
                                .all(|r| matches!(r, mixed::RowReceipt::Existing(_)))
                    })
            });
        if !(single || group)
            || source.table != table
            || source.table != original.table
            || source.record != original.record
            || source.version != original.version
            || source.repetition != 1
            || source.field == destination
            || member.table != table
            || !value.streams.is_empty()
            || member.owner
                != Some(crate::library::file_key(
                    self.session.as_ref().ok_or_else(unresolved)?,
                ))
        {
            return Err(unresolved());
        }
        self.validate_carry_group(&carry)?;
        let definition = carry.stamp.catalog.table(table).ok_or_else(unresolved)?;
        let field = definition.field(source.field).ok_or_else(unresolved)?;
        if field.kind != om_hbam::FieldKind::Normal
            || field.data_type != om_hbam::DataType::Container
            || field
                .decoded_options()
                .is_none_or(|o| o.options & 0x1_0000 != 0)
            || !carry.stamp.full
                && carry
                    .stamp
                    .access
                    .as_ref()
                    .is_none_or(|a| a.hides_field_id(table, source.field))
            || records::text_values(&records::stored_record(source.record, &original.row))
                .get(&source.field)
                .and_then(|v| om_data::containers::reference(v))
                != Some(member.member.clone())
        {
            return Err(unresolved());
        }
        // Extend only the original accepted row's destination footprint. The
        // source grants View, not Edit; its complete original row remains bound.
        if om_data::DataProvider::prepare_record_edit(
            self,
            &om_data::RecordEditRequest {
                baseline: om_data::ContainerEditBaseline {
                    layout,
                    record: i64::try_from(source.record).map_err(|_| unresolved())?,
                    version: Some(source.version),
                },
                cells: [destination].into_iter().collect(),
                fields: BTreeSet::new(),
            },
        )? != om_data::EditPreparation::Prepared
        {
            return Err(unresolved());
        }
        self.check_asset_acquisition(table, Some(destination))?;
        Ok(())
    }

    pub(crate) fn begin_stored_copy(
        &mut self,
        table: u64,
        destination: u64,
        value: &om_data::calc::Container,
    ) -> Result<(), DataError> {
        let source = value
            .library
            .as_ref()
            .and_then(|library| library.source)
            .ok_or_else(unresolved)?;
        let member = &value.library.as_ref().ok_or_else(unresolved)?.member;
        let (fingerprint, before) = self.asset_fingerprint(table, member)?;
        if before == 0 {
            return Err(unresolved());
        }
        let carry = self.scalar_carry.as_mut().ok_or_else(unresolved)?;
        if carry.pending_acquisition.is_some()
            || carry.existing.as_ref().is_none_or(|original| {
                !original
                    .observed
                    .iter()
                    .any(|(m, proof)| m == member && proof == &fingerprint)
            })
        {
            return Err(unresolved());
        }
        carry.pending_acquisition = Some(PendingAcquisition {
            kind: AcquisitionKind::StoredCopy(CopyProof {
                source,
                destination,
                member: member.clone(),
                fingerprint,
                before,
            }),
            target: None,
            bytes: Vec::new(),
            acknowledged: None,
        });
        Ok(())
    }

    pub(crate) fn acknowledge_stored_copy(
        &mut self,
        table: u64,
        member: &[u8],
        acknowledged_count: u32,
    ) -> Result<(), DataError> {
        if let Some(pending) = self
            .scalar_carry
            .as_mut()
            .and_then(|c| c.pending_acquisition.as_mut())
        {
            pending.acknowledged = Some((table, member.to_vec()));
        }
        let pending = self
            .scalar_carry
            .as_ref()
            .and_then(|c| c.pending_acquisition.clone())
            .ok_or_else(unresolved)?;
        let AcquisitionKind::StoredCopy(copy) = pending.kind else {
            return Err(unresolved());
        };
        let (now, count) = self.asset_fingerprint(table, member)?;
        if copy.source.table != table
            || copy.destination == copy.source.field
            || copy.member != member
            || now != copy.fingerprint
            || copy.before.checked_add(1) != Some(acknowledged_count)
            || count != acknowledged_count
        {
            return Err(unresolved());
        }
        self.acknowledge_root_acquisition(table, member);
        Ok(())
    }
}
