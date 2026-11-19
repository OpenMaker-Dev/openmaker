//! Carry ordinary never-submitted records, independently proving asset ownership.
//! The receipt and every owner's issued values precede closing the old sessions.
use super::*;
pub(crate) mod mixed;
mod stored_copy;
use om_data::host::security::RecordAction;
use om_data::{FileOutcome, SubmissionMark, Submitted};

/// Only caller-written fields belong here. Issued serial/clock/UUID cells
/// remain immutable creation state, never grants to edit those fields.
#[derive(Clone, Default)]
pub(crate) struct Footprint {
    tables: BTreeMap<u64, TableNeeds>,
    layouts: BTreeSet<u32>,
}

#[derive(Clone, Default)]
struct TableNeeds {
    create: bool,
    edit: bool,
    fields: BTreeSet<u64>,
}

impl Footprint {
    pub(crate) fn write(
        &mut self,
        table: u64,
        create: bool,
        fields: BTreeSet<u64>,
        layouts: &BTreeSet<u32>,
    ) {
        let needs = self.tables.entry(table).or_default();
        needs.create |= create;
        needs.edit |= !create;
        needs.fields.extend(fields);
        self.layouts.extend(layouts);
    }
    fn observe(&mut self, table: u64) {
        self.tables.entry(table).or_default();
    }
    fn extend(&mut self, other: Self) {
        for (table, add) in other.tables {
            let needs = self.tables.entry(table).or_default();
            needs.create |= add.create;
            needs.edit |= add.edit;
            needs.fields.extend(add.fields);
        }
        self.layouts.extend(other.layouts);
    }
}

#[derive(Clone)]
struct Stamp {
    creation: Option<u32>,
    catalog: om_hbam::Catalog,
    account: String,
    principal: Option<privileges::Principal>,
    access: Option<om_data::host::security::SetAccess>,
    schema: Option<Vec<om_hbam::OwnedNode>>,
    needs: Option<Footprint>,
    complete_tables: BTreeSet<u64>,
    full: bool,
    writable: bool,
}

impl Stamp {
    fn read(p: &NativeProvider) -> Self {
        let full = p.account_info.set_access.as_ref().map_or_else(
            || {
                p.privileges
                    .principal
                    .as_ref()
                    .is_some_and(|id| id.is_full_access())
                    && p.privileges.account.as_ref().is_some_and(|a| {
                        a.account_set_name == "[Full Access]" && a.record_access == Some(2)
                    })
            },
            |a| a.full_access(),
        );
        Self {
            creation: p.recovery_creation,
            catalog: p.catalog.clone(),
            account: p.account_name.clone(),
            principal: p.privileges.principal.clone(),
            access: p.account_info.set_access.clone(),
            schema: Self::schema(p),
            needs: p.scalar_footprint(),
            complete_tables: p
                .catalog
                .tables
                .iter()
                .filter(|t| {
                    p.calc
                        .schema
                        .as_ref()
                        .is_some_and(|s| !s.are_fields_pending(t.id))
                })
                .map(|t| t.id)
                .collect(),
            full,
            writable: !p.read_only(),
        }
    }

    fn schema(p: &NativeProvider) -> Option<Vec<om_hbam::OwnedNode>> {
        let tree = download_tree(&p.schema_downloads).ok()?;
        // Compare the owning definitions, including relationship predicates,
        // value lists, functions and data-source paths, not only field flags.
        // Canonical nodes avoid depending on download chunk boundaries.
        let mut catalogs: BTreeMap<_, _> = [3, 4, 16, 32, 33]
            .into_iter()
            .filter_map(|key| tree.node(&[&[key]]).map(|n| n.to_owned_node()))
            .map(|node| (node.segment.clone(), node))
            .collect();
        // Root's actual calculation catalogs are read separately in both
        // eager and on-demand mode. Unknown reads cannot prove an empty catalog.
        catalogs.extend(
            p.calc
                .recovery_catalogs
                .as_ref()?
                .iter()
                .cloned()
                .map(|node| (node.segment.clone(), node)),
        );
        let mut fields: Vec<_> = catalogs.into_values().collect();
        for table in &p.catalog.tables {
            for field in &table.fields {
                let mut node = tree
                    .node(&[&key_bytes(table.id), &[3], &[5], &key_bytes(field.id)])?
                    .to_owned_node();
                // The serial counter is mutable issued state, not a schema
                // change. Recovery carries its values and never draws again.
                if field
                    .decoded_options()
                    .is_some_and(|o| o.auto_entry_serial())
                {
                    node.remove(&[&[0x0c]]);
                }
                fields.push(node);
            }
        }
        Some(fields)
    }

    fn proven(&self) -> bool {
        use om_data::host::privilege_set::{AccessLevel, Allow};
        use om_data::host::security::{RecordView, SetAccess};
        if !self.writable || self.creation.is_none() {
            return false;
        }
        if self.full {
            return true;
        }
        let (Some(access @ SetAccess::Custom(rules)), Some(needs), Some(_)) =
            (&self.access, &self.needs, &self.principal)
        else {
            return false;
        };
        // Ordinary saves can defer conditional/missing rules to the host.
        // Replacement reservations need affirmative, unconditional own proof.
        if self.schema.is_none()
            || std::iter::once(&rules.default)
                .chain(rules.tables.values())
                .any(|t| {
                    [
                        &t.actions.create,
                        &t.actions.view,
                        &t.actions.edit,
                        &t.actions.delete,
                    ]
                    .iter()
                    .any(|a| matches!(a, Allow::Limited(_)))
                })
        {
            return false;
        }
        needs.layouts.iter().all(|id| {
            self.catalog.layouts.iter().any(|l| l.id == u64::from(*id))
                && access.shows_layout(u64::from(*id))
                && access.layout_records(u64::from(*id)) == AccessLevel::Modifiable
        }) && needs.tables.iter().all(|(id, needs)| {
            let Some(table) = self.catalog.table(*id) else {
                return false;
            };
            let key = om_hbam::slot::uint_segment(*id);
            let own = rules.table(&key);
            self.complete_tables.contains(id)
                && own.fields != AccessLevel::No
                && own
                    .field_levels
                    .values()
                    .all(|level| *level != AccessLevel::No)
                && matches!(access.record_view(&key), RecordView::All)
                && (!needs.create || access.may(&key, RecordAction::Create))
                && (!needs.edit || access.may(&key, RecordAction::Edit))
                && table
                    .fields
                    .iter()
                    .all(|f| !access.hides_field_id(*id, f.id))
                && needs.fields.iter().all(|f| {
                    table.field(*f).is_some()
                        && access.modifies(&key, &om_hbam::slot::uint_segment(*f))
                })
        })
    }

    // A library count is broader than a record write: it can disclose data
    // anywhere in the file. Ordinary saves' host-enforced fallback is no proof.
    fn library(&self, table: u64, acquire: bool) -> bool {
        if self.full {
            return true;
        }
        self.proven()
            && self.complete_tables.contains(&table)
            && self.catalog.table(table).is_some()
            && self.access.as_ref().is_some_and(|access| {
                !access.limits_viewing()
                    && (!acquire || access.may(&key_bytes(table), RecordAction::Edit))
            })
    }

    fn assets(&self, assets: &[asset_recovery::Asset]) -> bool {
        assets.iter().all(|a| {
            let ((table, _), owned) = a.ownership();
            self.library(table, owned > 0)
        })
    }

    fn extend(&mut self, p: &NativeProvider) -> bool {
        if p.transaction.is_some() {
            self.needs = match (self.needs.take(), p.scalar_footprint()) {
                (Some(mut needs), Some(add)) => {
                    needs.extend(add);
                    Some(needs)
                }
                _ => None,
            };
        }
        self.proven()
    }

    fn explicit_identity(&self) -> bool {
        self.principal.is_some() && self.access.is_some() && self.schema.is_some()
    }

    // Hydration can add a previously unread table's field catalog before its
    // first row participates. Keep every original definition byte: normalize
    // only entire catalogs that were absent, compare, then append that proof.
    fn admit_catalogs(&mut self, p: &NativeProvider) -> bool {
        let fresh = Self::read(p);
        let (Some(old_nodes), Some(new_nodes)) = (&self.schema, &fresh.schema) else {
            return false;
        };
        let old_fields: usize = self.catalog.tables.iter().map(|t| t.fields.len()).sum();
        let new_fields: usize = fresh.catalog.tables.iter().map(|t| t.fields.len()).sum();
        let Some(old_base) = old_nodes.len().checked_sub(old_fields) else {
            return false;
        };
        let Some(new_base) = new_nodes.len().checked_sub(new_fields) else {
            return false;
        };
        if self.creation != fresh.creation
            || self.account != fresh.account
            || self.principal != fresh.principal
            || self.access != fresh.access
            || !fresh.writable
            || self.full != fresh.full
            || old_nodes[..old_base] != new_nodes[..new_base]
        {
            return false;
        }
        let mut normalized = fresh.catalog.clone();
        let mut nodes = new_nodes[..new_base].to_vec();
        let mut offset = new_base;
        for table in &mut normalized.tables {
            let Some(old) = self.catalog.table(table.id) else {
                return false;
            };
            let Some(end) = offset
                .checked_add(table.fields.len())
                .filter(|end| *end <= new_nodes.len())
            else {
                return false;
            };
            if self.complete_tables.contains(&table.id) {
                nodes.extend_from_slice(&new_nodes[offset..end]);
            } else {
                if !old.fields.is_empty() {
                    return false;
                }
                table.fields.clear();
            }
            offset = end;
        }
        if normalized != self.catalog || nodes != *old_nodes {
            return false;
        }
        self.catalog = fresh.catalog;
        self.schema = fresh.schema;
        self.complete_tables = fresh.complete_tables;
        true
    }

    fn matches(&self, p: &NativeProvider) -> bool {
        self.matches_group(p, false)
    }

    fn matches_group(&self, p: &NativeProvider, custom_group: bool) -> bool {
        let s = Self::read(p);
        self.proven()
            && s.writable
            && s.creation == self.creation
            && s.catalog.tables == self.catalog.tables
            && s.account == self.account
            && if self.full && !custom_group {
                s.full
            } else {
                self.explicit_identity()
                    && s.explicit_identity()
                    && s.principal == self.principal
                    && s.access == self.access
                    && s.catalog == self.catalog
                    && s.schema == self.schema
            }
    }
}

#[derive(Clone)]
pub(crate) struct RootStart {
    pub(crate) rows: mixed::Rows,
    pub(crate) participant: bool,
    upload: Option<PendingAcquisition>,
    mark: SubmissionMark,
    stamp: Stamp,
    identity: om_data::file_refs::FileIdentity,
    pub(crate) existing: Option<ExistingRoot>,
    pub(crate) row: Option<mixed::RowReceipt>,
}

pub(crate) struct ExistingAttachment {
    pub(crate) record: u64,
    pub(crate) version: u64,
    pub(crate) field: Option<u64>,
}

/// The stored original is observed state, never acquired container ownership.
#[derive(Clone)]
pub(crate) struct ExistingRoot {
    pub(crate) table: u64,
    pub(crate) record: u64,
    pub(crate) version: u64,
    row: om_hbam::OwnedNode,
    library: Option<Vec<u8>>,
    observed: Vec<(Vec<u8>, asset_recovery::Fingerprint)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CarryKind {
    RefusedGroup,
    SingleNewRoot,
    SingleExistingRoot,
    PreparedExistingGroup,
    PreparedMixedGroup,
}

impl RootStart {
    pub(crate) fn retain_record_edit(self, provider: &mut NativeProvider) -> Result<(), DataError> {
        if let Some(carry) = provider.scalar_carry.as_ref() {
            if carry.window != provider.window || carry.identity != self.identity {
                return Err(unresolved());
            }
            let mut stamp = carry.stamp.clone();
            if !stamp.extend(provider) {
                return Err(unresolved());
            }
            stamp
                .needs
                .as_mut()
                .ok_or_else(unresolved)?
                .extend(self.stamp.needs.clone().ok_or_else(unresolved)?);
            if !stamp.proven() {
                return Err(unresolved());
            }
            provider.scalar_carry.as_mut().ok_or_else(unresolved)?.stamp = stamp;
        }
        provider
            .transaction
            .as_mut()
            .ok_or_else(unresolved)?
            .single_root_start = Some(self);
        Ok(())
    }

    pub(crate) fn extend_record_edit(
        &mut self,
        provider: &NativeProvider,
        table: u64,
        fields: BTreeSet<u64>,
        layout: u32,
    ) -> bool {
        if self
            .mark
            .until(om_data::DataProvider::submission_mark(provider))
            != Submitted::Nothing
            || provider.external.root_identity() != Some(&self.identity)
            || !self.stamp.extend(provider)
        {
            return false;
        }
        let Some(needs) = self.stamp.needs.as_mut() else {
            return false;
        };
        needs.write(table, false, fields, &[layout].into_iter().collect());
        self.stamp.proven()
    }
}

impl CarryKind {
    fn single(self) -> bool {
        matches!(self, Self::SingleNewRoot | Self::SingleExistingRoot)
    }
    fn prepared(self) -> bool {
        self.single() || self.group()
    }
    fn group(self) -> bool {
        matches!(self, Self::PreparedExistingGroup | Self::PreparedMixedGroup)
    }
    fn for_root(existing: &Option<ExistingRoot>) -> Self {
        if existing.is_some() {
            Self::SingleExistingRoot
        } else {
            Self::SingleNewRoot
        }
    }
}

// Metadata can be complete while Root has not yet acquired its attachment.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RootAcquisition {
    Ready,
    AwaitingField,
    AwaitingAcknowledgement,
}

#[derive(Clone)]
struct PendingAcquisition {
    kind: AcquisitionKind,
    target: Option<mixed::Acquisition>,
    // Kept as evidence after a lost reply, never automatically replayed.
    #[allow(dead_code)]
    bytes: Vec<u8>,
    acknowledged: Option<(u64, Vec<u8>)>,
}

#[derive(Clone)]
enum AcquisitionKind {
    Upload,
    StoredCopy(stored_copy::CopyProof),
}

// Declared before joining: a failed preparation cannot leave an eligible
// prefix of an otherwise incomplete group. Mixed intents are per logical row;
// legacy existing-only groups retain their stricter one-row owner proof.
#[derive(Clone)]
struct OwnerIntent {
    token: Option<u64>,
    row: Option<mixed::RowIntent>,
    // Failed creations retain their caller input as evidence, never replay it.
    input: Option<om_data::ScriptWrite>,
    table: u64,
    wire: u64,
    record: u64,
    version: u64,
    occurrence: u64,
    identity: Option<om_data::file_refs::FileIdentity>,
}

#[derive(Clone)]
struct Owner {
    rows: mixed::Rows,
    table: u64,
    routes: Vec<(u64, u64)>,
    identity: om_data::file_refs::FileIdentity,
    stamp: Stamp,
    records: transaction::ScalarRecords,
    assets: Vec<asset_recovery::Asset>,
    existing: Option<ExistingRoot>,
    pending_acquisition: Option<PendingAcquisition>,
    row: Option<mixed::RowReceipt>,
}

impl Owner {
    // Even an error must retain the owner's pre-wire payload. An absent ACK
    // proves no acquisition count and cannot restore a Root-only receipt.
    fn collect_upload(
        &mut self,
        pending: Option<PendingAcquisition>,
        records: Option<transaction::ScalarRecords>,
        assets: Result<Vec<asset_recovery::Asset>, DataError>,
    ) -> bool {
        self.pending_acquisition = pending;
        let mut complete = true;
        if let Some(records) = records {
            self.records = records;
        } else {
            complete = false;
        }
        if let Ok(assets) = assets {
            self.assets = assets;
        } else {
            complete = false;
        }
        complete
    }
}

pub(crate) struct Receipt {
    mark: SubmissionMark,
    versions: Vec<(u64, u64, u64)>,
    assets: Vec<asset_recovery::Asset>,
    owners: BTreeMap<u64, Vec<asset_recovery::Asset>>,
    stamp: Stamp,
    owner_stamps: BTreeMap<u64, Stamp>,
}

type DetachedCounts = BTreeMap<Option<u64>, BTreeMap<(u64, Vec<u8>), i32>>;

/// An all-participant never-submitted receipt and immutable issued values.
/// A failed explicit reopen retains this group; it never turns into a replay.
#[derive(Clone)]
pub(crate) struct ScalarCarry {
    kind: CarryKind,
    root_acquisition: RootAcquisition,
    // True only after cleanup authority has left the live transaction.
    assets_detached: bool,
    // Detached counts without a fingerprint remain evidence, never release authority.
    detached_counts: DetachedCounts,
    // An op88 whose acknowledgement/fingerprint is missing retains its bytes.
    pending_acquisition: Option<PendingAcquisition>,
    window: u32,
    mark: SubmissionMark,
    complete: bool,
    stamp: Stamp,
    identity: om_data::file_refs::FileIdentity,
    root: transaction::ScalarRecords,
    existing: Option<ExistingRoot>,
    assets: Vec<asset_recovery::Asset>,
    occurrences: Vec<u64>,
    versions: Vec<(u64, u64, u64)>,
    owners: Vec<Owner>,
    intents: Vec<OwnerIntent>,
    topology: Option<external::RecoveryTopology>,
    row: Option<mixed::RowReceipt>,
}

impl ScalarCarry {
    fn prepared_membership(&self) -> bool {
        if self.kind == CarryKind::PreparedMixedGroup {
            return self.mixed_membership();
        }
        !self.owners.is_empty()
            && self.owners.len() <= external::MAX_FILES
            && self.intents.len() == self.owners.len()
            && self.owners.iter().enumerate().all(|(index, owner)| {
                let intent = &self.intents[index];
                intent.table == owner.table
                    && intent.identity.as_ref() == Some(&owner.identity)
                    && self.occurrences.contains(&intent.occurrence)
                    && owner.routes.contains(&(intent.table, intent.wire))
                    && owner.existing.as_ref().is_some_and(|e| {
                        (e.table, e.record, e.version)
                            == (intent.wire, intent.record, intent.version)
                    })
                    && self.owners[..index]
                        .iter()
                        .all(|prior| prior.table != owner.table && prior.identity != owner.identity)
            })
    }

    fn unresolved(&self) -> DataError {
        if self.kind == CarryKind::SingleExistingRoot {
            DataError::with_code(3, "Unresolved: the existing record and its attachment could not be revalidated. All drafts and issued values are kept; nothing was saved.")
        } else if self.kind.single() {
            DataError::with_code(3, "Unresolved: the new record and its attachment could not be revalidated. All drafts and issued values are kept; nothing was saved.")
        } else {
            unresolved()
        }
    }

    fn strict(&self) -> bool {
        self.kind.prepared() || self.custom_group()
    }
    fn custom_group(&self) -> bool {
        !self.stamp.full || self.owners.iter().any(|o| !o.stamp.full)
    }
    pub(crate) fn has_assets(&self) -> bool {
        self.pending_acquisition.is_some()
            || !self.assets.is_empty()
            || self
                .owners
                .iter()
                .any(|o| o.pending_acquisition.is_some() || !o.assets.is_empty())
    }
    fn asset_mutation(
        &mut self,
        table: u64,
        member: &[u8],
        mut edit: impl FnMut(&mut asset_recovery::Asset) -> Result<(), DataError>,
    ) -> Result<(), DataError> {
        for asset in &mut self.assets {
            if asset.names(table, member) {
                edit(asset)?;
            }
        }
        for part in &mut self.owners {
            if let Some((_, wire)) = part.routes.iter().find(|(client, _)| *client == table) {
                for asset in &mut part.assets {
                    if asset.names(*wire, member) {
                        edit(asset)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn window(&self) -> u32 {
        self.window
    }
}

fn unresolved() -> DataError {
    DataError::with_code(3, "Unresolved: the related save could not be revalidated. All drafts and issued values are kept; nothing was saved.")
}

impl NativeProvider {
    pub(crate) fn prepared_attachment_group(&self) -> bool {
        self.scalar_carry.as_ref().is_some_and(|c| c.kind.group())
    }

    fn prepared_joined_membership(&self, c: &ScalarCarry) -> bool {
        let mut expected = BTreeSet::new();
        for owner in &c.owners {
            let Some(route) = self.external.route(owner.table) else {
                return false;
            };
            if self.external.route_identity(route).as_ref() != Some(&owner.identity)
                || !expected.insert(route.file_index())
            {
                return false;
            }
        }
        let mut joined = BTreeSet::new();
        for table in self.owner_transaction.owner_tables() {
            let Some(route) = self.external.route(*table) else {
                return false;
            };
            if !joined.insert(route.file_index()) {
                return false;
            }
        }
        joined == expected
    }

    fn prepared_group_shape(&self, c: &ScalarCarry, retaining_drafts: bool) -> bool {
        if c.kind == CarryKind::PreparedMixedGroup {
            return self.mixed_group_shape(c, retaining_drafts);
        }
        c.prepared_membership()
            && self.prepared_joined_membership(c)
            && self.attachment_group_root_shape(
                retaining_drafts
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
                            Ok(owner.attachment_part_shape()
                                && owner
                                    .scalar_records()
                                    .is_some_and(|r| r.same_records(&p.records)))
                        })
                        .unwrap_or(false)
                })
            })
    }

    fn validate_attachment_original(&mut self, original: &ExistingRoot) -> Result<(), DataError> {
        if let Some(identity) = &original.library {
            if self.attachment_library_identity(original.table)?.as_ref() != Some(identity) {
                return Err(unresolved());
            }
        }
        let row = self
            .read_record(original.table, original.record)?
            .ok_or_else(unresolved)?;
        if row != original.row
            || records::mod_count(&records::stored_record(original.record, &row))?
                != original.version
        {
            return Err(unresolved());
        }
        for (member, proof) in &original.observed {
            let (now, count) = self.asset_fingerprint(original.table, member)?;
            if &now != proof || count == 0 {
                return Err(unresolved());
            }
        }
        Ok(())
    }

    fn begin_related_first(
        &mut self,
        root: om_data::ContainerEditBaseline,
    ) -> Result<bool, DataError> {
        if !self.prepare_attachment_root(root.layout, root.record, root.version, None)? {
            return Ok(false);
        }
        let start = self
            .transaction
            .as_ref()
            .and_then(|t| t.single_root_start.clone())
            .ok_or_else(unresolved)?;
        let records = self.scalar_root_records().ok_or_else(unresolved)?;
        self.scalar_carry = Some(ScalarCarry {
            row: None,
            kind: CarryKind::SingleExistingRoot,
            root_acquisition: RootAcquisition::AwaitingField,
            assets_detached: false,
            detached_counts: BTreeMap::new(),
            pending_acquisition: None,
            window: self.window,
            mark: start.mark,
            complete: true,
            stamp: start.stamp,
            identity: start.identity,
            root: records,
            existing: start.existing,
            assets: Vec::new(),
            occurrences: Vec::new(),
            versions: Vec::new(),
            owners: Vec::new(),
            intents: Vec::new(),
            topology: None,
        });
        Ok(true)
    }

    // The field is chosen only now, under the original before-work proof.
    pub(crate) fn prepare_awaiting_root_field(
        &mut self,
        layout: u32,
        record: i64,
        version: Option<u64>,
        field: &str,
    ) -> Result<Option<bool>, DataError> {
        let Some(mut carry) = self
            .scalar_carry
            .as_ref()
            .filter(|c| c.kind.group() && c.root_acquisition == RootAcquisition::AwaitingField)
            .cloned()
        else {
            return Ok(None);
        };
        self.refuse_while_unsettled()?;
        if carry.window != self.window
            || !self.scalar_session_alone()
            || carry.assets_detached
            || !self.prepared_group_shape(&carry, false)
            || !carry.assets.is_empty()
            || carry.pending_acquisition.is_some()
            || carry
                .owners
                .iter()
                .any(|o| o.assets.is_empty() || o.assets.iter().any(|a| !a.ownership_known()))
        {
            return Err(unresolved());
        }
        self.validate_carry_group(&carry)?;
        let original = carry.existing.as_ref().ok_or_else(unresolved)?;
        if Some(original.record) != u64::try_from(record).ok()
            || Some(original.version) != version
            || carry
                .stamp
                .needs
                .as_ref()
                .is_none_or(|n| !n.layouts.contains(&layout))
        {
            return Err(unresolved());
        }
        let (table, _) = self.record_plan(layout)?;
        let field = table
            .field_by_name(field)
            .filter(|f| {
                f.data_type == om_hbam::DataType::Container
                    && f.kind == om_hbam::FieldKind::Normal
                    && f.decoded_options()
                        .is_some_and(|o| o.options & 0x1_0000 == 0)
            })
            .ok_or_else(unresolved)?
            .id;
        if table.id != original.table {
            return Err(unresolved());
        }
        carry.stamp.needs.as_mut().ok_or_else(unresolved)?.write(
            table.id,
            false,
            [field].into_iter().collect(),
            &[layout].into_iter().collect(),
        );
        if !carry.stamp.proven() || !carry.stamp.library(table.id, true) {
            return Err(unresolved());
        }
        self.extend_attachment_root_field(table.id, original.record, field)?;
        carry.root = self.scalar_root_records().ok_or_else(unresolved)?;
        carry.root_acquisition = RootAcquisition::AwaitingAcknowledgement;
        self.scalar_carry = Some(carry);
        Ok(Some(true))
    }

    pub(crate) fn prepare_related_attachment(
        &mut self,
        occurrence: u64,
        table: u64,
        record: i64,
        version: Option<u64>,
        field: u64,
        root: Option<om_data::ContainerEditBaseline>,
    ) -> Result<bool, DataError> {
        self.refuse_while_unsettled()?;
        if let Some(prepared) =
            self.prepare_mixed_container_target(table, record, version, field, root)?
        {
            return Ok(prepared);
        }
        if self.external.route(table).is_none() {
            return Ok(false);
        }
        if self.scalar_carry.is_none() {
            let Some(root) = root else { return Ok(false) };
            if !self.begin_related_first(root)? {
                return Ok(false);
            }
        }
        let Some(mut carry) = self
            .scalar_carry
            .as_ref()
            .filter(|c| {
                c.kind == CarryKind::SingleExistingRoot
                    || c.kind == CarryKind::PreparedExistingGroup
            })
            .cloned()
        else {
            return Ok(false);
        };
        // Same-file entry keeps its ordinary path without promising this
        // narrowly external-owner recovery shape.
        let Some(route) = self.external.route(table) else {
            return Ok(false);
        };
        self.validate_carry_group(&carry)?;
        if let Some(root) = root {
            if carry.existing.as_ref().is_none_or(|e| {
                Some(e.record) != u64::try_from(root.record).ok() || Some(e.version) != root.version
            }) || carry
                .stamp
                .needs
                .as_ref()
                .is_none_or(|n| !n.layouts.contains(&root.layout))
            {
                return Err(unresolved());
            }
        }
        let record = u64::try_from(record).map_err(|_| unresolved())?;
        let version = version.ok_or_else(|| {
            DataError::with_code(306, "The accepted related record version is unavailable.")
        })?;
        if self
            .pinned
            .get(&(self.window, table, record))
            .is_some_and(|v| *v != Some(version))
        {
            return Err(DataError::with_code(
                306,
                "The accepted related record version changed.",
            ));
        }
        let schema = self.calc.schema.as_ref().ok_or_else(unresolved)?;
        if schema.table_of(occurrence).is_none_or(|t| t.id != table) {
            return Err(unresolved());
        }
        let identity = self.external.route_identity(route);
        if carry.kind == CarryKind::PreparedExistingGroup {
            for owner in &carry.owners {
                let old = self.external.route(owner.table).ok_or_else(unresolved)?;
                let same_route = old.same_file(route);
                let same_identity = identity.as_ref() == Some(&owner.identity);
                if same_route || same_identity {
                    if !same_route
                        || !same_identity
                        || owner.existing.as_ref().is_none_or(|e| {
                            (e.table, e.record, e.version) != (route.table, record, version)
                        })
                        || !owner.routes.contains(&(table, route.table))
                    {
                        return Err(unresolved());
                    }
                    if !carry.occurrences.contains(&occurrence) {
                        carry.occurrences.push(occurrence);
                    }
                    self.scalar_carry = Some(carry);
                    return Ok(true);
                }
            }
        } else if !self.owner_transaction.alone() {
            return Err(unresolved());
        }
        if carry.assets_detached
            || carry.pending_acquisition.is_some()
            || carry.owners.len() >= external::MAX_FILES
            || (carry.assets.is_empty() && carry.root_acquisition != RootAcquisition::AwaitingField)
        {
            return Err(unresolved());
        }
        // Persist intended membership before a join can fail. No old subset
        // can recover, release assets or claim an acknowledged new owner.
        carry.kind = CarryKind::PreparedExistingGroup;
        carry.complete = false;
        carry.intents.push(OwnerIntent {
            token: None,
            input: None,
            row: None,
            table,
            wire: route.table,
            record,
            version,
            occurrence,
            identity: identity.clone(),
        });
        if !carry.occurrences.contains(&occurrence) {
            carry.occurrences.push(occurrence);
        }
        self.scalar_carry = Some(carry.clone());
        let identity = identity.ok_or_else(unresolved)?;
        self.join_owner_transaction(table, route)?;
        let (wire, start, records) = self.external.with_owner(route, |owner, wire| {
            let start = owner.prepare_attachment_part(
                wire,
                record,
                version,
                field,
                carry.mark,
                identity.clone(),
            )?;
            Ok((wire, start, owner.scalar_records().ok_or_else(unresolved)?))
        })?;
        carry.owners.push(Owner {
            rows: mixed::Rows::default(),
            row: None,
            table,
            routes: self.external.route_tables(route),
            identity: start.identity,
            stamp: start.stamp,
            records,
            assets: Vec::new(),
            existing: start.existing,
            pending_acquisition: None,
        });
        // Retain even malformed post-join evidence before refusing it.
        carry.complete = carry.prepared_membership()
            && carry
                .owners
                .last()
                .is_some_and(|p| p.routes.contains(&(table, wire)))
            && self.prepared_joined_membership(&carry);
        self.scalar_carry = Some(carry.clone());
        if !carry.complete {
            return Err(unresolved());
        }
        self.pinned
            .insert((self.window, table, record), Some(version));
        self.scalar_carry = Some(carry);
        Ok(true)
    }

    /// Called after BOTH success and error, before returning the upload result.
    pub(crate) fn collect_attachment_part(&mut self, table: u64) {
        if self.collect_mixed_part(table) {
            return;
        }
        if !self.prepared_attachment_group() {
            return;
        }
        let evidence = self.external.route(table).and_then(|route| {
            self.external
                .with_owner(route, |owner, _| {
                    let pending = owner
                        .transaction
                        .as_ref()
                        .and_then(|t| t.single_root_start.as_ref())
                        .and_then(|s| s.upload.clone());
                    Ok((pending, owner.scalar_records(), owner.carry_assets()))
                })
                .ok()
        });
        if let Some(c) = self.scalar_carry.as_mut() {
            if let Some(p) = c
                .owners
                .iter_mut()
                .find(|p| p.routes.iter().any(|(client, _)| *client == table))
            {
                if let Some((pending, records, assets)) = evidence {
                    c.complete &= p.collect_upload(pending, records, assets);
                } else {
                    c.complete = false;
                }
            } else {
                c.complete = false;
            }
        }
    }

    fn refresh_attachment_group(&mut self) {
        let records = self.scalar_root_records();
        let assets = self.carry_assets();
        if let Some(mut c) = self.scalar_carry.take() {
            if let Some(records) = records {
                c.root = records;
            } else {
                c.complete = false;
            }
            if let Ok(assets) = assets {
                c.assets = assets;
            } else {
                c.complete = false;
            }
            c.complete &= c.stamp.extend(self);
            self.scalar_carry = Some(c);
        }
    }

    /// Add attachment observations to a scalar-first baseline without
    /// replacing its original row/version or principal/schema stamp.
    pub(crate) fn extend_existing_root_attachment(
        &mut self,
        layout: u32,
        record: i64,
        version: Option<u64>,
        field: &str,
    ) -> Result<Option<bool>, DataError> {
        let Some(mut start) = self
            .transaction
            .as_ref()
            .and_then(|t| t.single_root_start.clone())
            .filter(|s| s.existing.is_some())
        else {
            return Ok(None);
        };
        let original = start.existing.clone().ok_or_else(unresolved)?;
        if u64::try_from(record).ok() != Some(original.record) || version != Some(original.version)
        {
            return Err(DataError::with_code(
                306,
                "The accepted attachment record changed.",
            ));
        }
        let (table, _) = self.record_plan(layout)?;
        if table.id != original.table {
            return Err(unresolved());
        }
        let field = table
            .field_by_name(field)
            .filter(|f| {
                f.data_type == om_hbam::DataType::Container
                    && f.kind == om_hbam::FieldKind::Normal
                    && f.decoded_options()
                        .is_some_and(|o| o.options & 0x1_0000 == 0)
            })
            .ok_or_else(unresolved)?
            .id;
        if !start.extend_record_edit(self, table.id, [field].into_iter().collect(), layout)
            || !start.stamp.library(table.id, true)
        {
            return Ok(Some(false));
        }
        let observed = self.existing_attachment_start_at(
            &table,
            ExistingAttachment {
                record: original.record,
                version: original.version,
                field: Some(field),
            },
            Some(layout),
            start.mark,
            start.identity.clone(),
        )?;
        let Some(observed) = observed.and_then(|s| s.existing) else {
            // No asset has yet been taken. Keep ordinary entry available, but
            // do not let an unproved upload inherit this scalar receipt.
            if self.scalar_carry.is_none() && self.owner_transaction.alone() {
                if let Some(t) = &mut self.transaction {
                    t.single_root_start = None;
                }
            }
            return Ok(Some(false));
        };
        if observed.row != original.row {
            return Err(unresolved());
        }
        start.existing = Some(observed.clone());
        start.row = Some(mixed::RowReceipt::Existing(observed.clone()));
        self.transaction
            .as_mut()
            .ok_or_else(unresolved)?
            .single_root_start = Some(start.clone());
        if let Some(c) = &mut self.scalar_carry {
            c.existing = Some(observed.clone());
            c.row = Some(mixed::RowReceipt::Existing(observed));
            c.stamp = start.stamp;
        }
        Ok(Some(true))
    }

    pub(crate) fn existing_attachment_start(
        &mut self,
        table: &om_hbam::Table,
        record: u64,
        version: u64,
        field: Option<u64>,
        layout: u32,
    ) -> Result<Option<RootStart>, DataError> {
        let mark = self.single_root_baseline().ok_or_else(unresolved)?;
        let identity = self
            .external
            .root_identity()
            .cloned()
            .ok_or_else(unresolved)?;
        self.existing_attachment_start_at(
            table,
            ExistingAttachment {
                record,
                version,
                field,
            },
            Some(layout),
            mark,
            identity,
        )
    }

    pub(crate) fn existing_attachment_start_at(
        &mut self,
        table: &om_hbam::Table,
        input: ExistingAttachment,
        layout: Option<u32>,
        mark: SubmissionMark,
        identity: om_data::file_refs::FileIdentity,
    ) -> Result<Option<RootStart>, DataError> {
        self.existing_edit_start_at(table, input, layout, mark, identity, true)
    }

    pub(crate) fn existing_edit_start_at(
        &mut self,
        table: &om_hbam::Table,
        input: ExistingAttachment,
        layout: Option<u32>,
        mark: SubmissionMark,
        identity: om_data::file_refs::FileIdentity,
        library_needed: bool,
    ) -> Result<Option<RootStart>, DataError> {
        let ExistingAttachment {
            record,
            version,
            field,
        } = input;
        self.calc_schema_cached()?;
        let mut stamp = Stamp::read(self);
        stamp.needs.as_mut().ok_or_else(unresolved)?.write(
            table.id,
            false,
            field.into_iter().collect(),
            &layout.into_iter().collect(),
        );
        if !stamp.proven()
            || !stamp.explicit_identity()
            || stamp.schema.is_none()
            || (library_needed && !stamp.library(table.id, true))
        {
            return Ok(None);
        }
        let library = if library_needed {
            let Ok(library) = self.attachment_library_identity(table.id) else {
                return Ok(None);
            };
            library
        } else {
            None
        };
        let row = self.read_record(table.id, record)?.ok_or_else(|| {
            DataError::with_code(101, "The attachment's record no longer exists.")
        })?;
        let stored = records::stored_record(record, &row);
        if records::mod_count(&stored)? != version {
            return Err(DataError::with_code(
                306,
                "The record changed before entry.",
            ));
        }
        let mut observed = Vec::new();
        for member in containers::references(table, &records::text_values(&stored), None)
            .into_iter()
            .filter(|_| library_needed)
        {
            let Ok((proof, count)) = self.asset_fingerprint(table.id, &member) else {
                // No upload has begun: ordinary entry may continue, but an
                // unidentifiable stored original cannot authorize recovery.
                return Ok(None);
            };
            if count == 0 {
                return Ok(None);
            }
            observed.push((member, proof));
        }
        if mark.until(om_data::DataProvider::submission_mark(self)) != Submitted::Nothing {
            return Err(unresolved());
        }
        Ok(Some(RootStart {
            rows: mixed::Rows::default(),
            row: None,
            participant: false,
            upload: None,
            mark,
            stamp,
            identity,
            existing: Some(ExistingRoot {
                table: table.id,
                record,
                version,
                row,
                library,
                observed,
            }),
        }))
    }

    pub(crate) fn begin_single_root_proof(
        &mut self,
        table: u64,
        fields: BTreeSet<u64>,
        layout: Option<u32>,
    ) {
        if !self.owner_transaction.alone() || !self.scalar_session_alone() {
            return;
        }
        let Some(mark) = self.single_root_baseline() else {
            return;
        };
        if mark.until(om_data::DataProvider::submission_mark(self)) != Submitted::Nothing
            || self.calc_schema_cached().is_err()
        {
            return;
        }
        let mut stamp = Stamp::read(self);
        if let Some(needs) = &mut stamp.needs {
            needs.write(table, true, fields, &layout.into_iter().collect());
        }
        let Some(identity) = self.external.root_identity().cloned() else {
            return;
        };
        if stamp.proven() && stamp.explicit_identity() {
            if let Some(t) = &mut self.transaction {
                t.single_root_start = Some(RootStart {
                    rows: mixed::Rows::default(),
                    row: Some(mixed::RowReceipt::creating(table)),
                    participant: false,
                    upload: None,
                    mark,
                    stamp,
                    identity,
                    existing: None,
                });
            }
        }
    }

    // No wire reads: the acquisition hook has already retained the ACKed tree.
    pub(crate) fn remember_single_root(&mut self) {
        if self.prepared_attachment_group() {
            self.refresh_attachment_group();
            return;
        }
        if self.scalar_carry.as_ref().is_some_and(|c| !c.kind.single())
            || self.asset_settlement.is_some()
            || self.single_root_shape().is_none()
        {
            return;
        }
        let Some(start) = self
            .transaction
            .as_ref()
            .and_then(|t| t.single_root_start.clone())
        else {
            return;
        };
        if start
            .mark
            .until(om_data::DataProvider::submission_mark(self))
            != Submitted::Nothing
        {
            return;
        }
        let Ok(assets) = self.cached_root_assets(false) else {
            return;
        };
        if assets.is_empty() {
            return;
        }
        let Some(root) = self.scalar_root_records() else {
            return;
        };
        let mut stamp = self
            .scalar_carry
            .as_ref()
            .map_or(start.stamp, |c| c.stamp.clone());
        if !stamp.extend(self) || !stamp.assets(&assets) {
            return;
        }
        if let Some(c) = &mut self.scalar_carry {
            c.root = root;
            c.assets = assets;
            c.stamp = stamp;
            return;
        }
        self.scalar_carry = Some(ScalarCarry {
            row: start.row.clone(),
            root_acquisition: RootAcquisition::Ready,
            kind: CarryKind::for_root(&start.existing),
            assets_detached: false,
            detached_counts: BTreeMap::new(),
            pending_acquisition: None,
            window: self.window,
            mark: start.mark,
            complete: true,
            stamp,
            identity: start.identity,
            root,
            existing: start.existing.clone(),
            assets,
            occurrences: Vec::new(),
            versions: Vec::new(),
            owners: Vec::new(),
            intents: Vec::new(),
            topology: None,
        });
    }

    /// Acquisition-neutral construction: a first op87 copy has no upload
    /// payload or credit. Its own typed pending intent must precede the wire.
    pub(crate) fn ensure_single_root_receipt(&mut self, table: u64, field: u64) -> bool {
        if self
            .single_root_shape()
            .is_none_or(|(wire, _)| wire != table)
        {
            return false;
        }
        let Some(start) = self
            .transaction
            .as_ref()
            .and_then(|t| t.single_root_start.clone())
        else {
            return false;
        };
        if start
            .mark
            .until(om_data::DataProvider::submission_mark(self))
            != Submitted::Nothing
        {
            return false;
        }
        if self.scalar_carry.is_none() {
            let Some(root) = self.scalar_root_records() else {
                return false;
            };
            let Ok(assets) = self.cached_root_assets(false) else {
                return false;
            };
            let mut stamp = start.stamp;
            if !stamp.extend(self) || !stamp.library(table, true) {
                return false;
            }
            let Some(needs) = stamp.needs.as_mut() else {
                return false;
            };
            needs.write(
                table,
                start.existing.is_none(),
                [field].into_iter().collect(),
                &BTreeSet::new(),
            );
            if !stamp.proven() {
                return false;
            }
            self.scalar_carry = Some(ScalarCarry {
                row: start.row.clone(),
                root_acquisition: RootAcquisition::Ready,
                kind: CarryKind::for_root(&start.existing),
                assets_detached: false,
                detached_counts: BTreeMap::new(),
                pending_acquisition: None,
                window: self.window,
                mark: start.mark,
                complete: true,
                stamp,
                identity: start.identity,
                root,
                existing: start.existing.clone(),
                assets,
                occurrences: Vec::new(),
                versions: Vec::new(),
                owners: Vec::new(),
                intents: Vec::new(),
                topology: None,
            });
        }
        self.scalar_carry
            .as_ref()
            .is_some_and(|c| c.kind.single() && c.window == self.window)
    }

    pub(crate) fn begin_single_upload(&mut self, table: u64, field: u64, payload: &[u8]) {
        if self.attachment_part_shape() {
            if let Some(start) = self
                .transaction
                .as_mut()
                .and_then(|t| t.single_root_start.as_mut())
            {
                if start.upload.is_none() {
                    start.upload = Some(PendingAcquisition {
                        kind: AcquisitionKind::Upload,
                        target: start.rows.acquisition,
                        bytes: payload.to_vec(),
                        acknowledged: None,
                    });
                }
            }
            return;
        }
        if let Some(c) = self.scalar_carry.as_mut().filter(|c| c.kind.group()) {
            c.pending_acquisition = Some(PendingAcquisition {
                kind: AcquisitionKind::Upload,
                target: None,
                bytes: payload.to_vec(),
                acknowledged: None,
            });
            return;
        }
        if !self.ensure_single_root_receipt(table, field) {
            return;
        }
        if let Some(c) = self.scalar_carry.as_mut().filter(|c| c.kind.single()) {
            // An uncertain older upload cannot be overwritten by a new one.
            if c.pending_acquisition.is_none() {
                c.pending_acquisition = Some(PendingAcquisition {
                    kind: AcquisitionKind::Upload,
                    target: None,
                    bytes: payload.to_vec(),
                    acknowledged: None,
                });
            }
        }
    }

    pub(crate) fn acknowledge_root_acquisition(&mut self, table: u64, member: &[u8]) {
        if self.attachment_part_shape() {
            let asset = self.acquired_asset(table, member, 1);
            if let Some(t) = self.transaction.as_mut() {
                *t.root_acquisitions
                    .entry((table, member.to_vec()))
                    .or_default() += 1;
                if let Some(start) = t.single_root_start.as_mut() {
                    if let Some(pending) = start.upload.as_mut() {
                        pending.acknowledged = Some((table, member.to_vec()));
                    }
                    if start
                        .upload
                        .as_ref()
                        .is_some_and(|pending| pending.target == start.rows.acquisition)
                        && asset.as_ref().is_some_and(|a| {
                            start
                                .existing
                                .as_ref()
                                .and_then(|e| e.library.as_deref())
                                .is_none_or(|id| a.same_library(id))
                        })
                    {
                        start.upload = None;
                    }
                }
            }
            return;
        }
        if let Some(t) = &mut self.transaction {
            *t.root_acquisitions
                .entry((table, member.to_vec()))
                .or_default() += 1;
        }
        if let Some(pending) = self
            .scalar_carry
            .as_mut()
            .filter(|c| c.kind.prepared())
            .and_then(|c| c.pending_acquisition.as_mut())
        {
            pending.acknowledged = Some((table, member.to_vec()));
        }
        // Clear the pending bytes only once the ACK's immutable proof exists.
        if self.acquired_asset(table, member, 1).is_some_and(|asset| {
            self.scalar_carry
                .as_ref()
                .and_then(|c| c.existing.as_ref())
                .and_then(|e| e.library.as_deref())
                .is_none_or(|id| asset.same_library(id))
        }) {
            if let Some(c) = self.scalar_carry.as_mut().filter(|c| c.kind.prepared()) {
                c.pending_acquisition = None;
                if c.root_acquisition == RootAcquisition::AwaitingAcknowledgement {
                    c.root_acquisition = RootAcquisition::Ready;
                }
            }
        }
        self.remember_single_root();
    }

    /// Closing the old provider must not discard another window's held or
    /// unresolved work, even when this window's own save sent no data.
    pub(crate) fn scalar_session_alone(&self) -> bool {
        self.record_sessions.is_empty()
            && self.entered_over.is_none()
            && self.unsettled.is_empty()
            && self.carried.is_empty()
            && self.unrecreated.is_empty()
    }

    pub(crate) fn scalar_reconnect_blocked(&self) -> bool {
        if self.scalar_carry.is_some() {
            return self.scalar_carry_window().is_none();
        }
        if self.transaction_kind() == Some(transaction::TransactionKind::Record)
            && self
                .transaction
                .as_ref()
                .is_some_and(|t| t.upload_attempted || t.containers.values().any(|n| *n > 0))
        {
            return true;
        }
        if self.transaction_kind() != Some(transaction::TransactionKind::Record)
            || self.owner_transaction.owner_tables().is_empty()
        {
            return false;
        }
        // Failure to capture complete metadata/asset proof is not permission
        // to fall through to Root-only recreation and drop a related identity.
        self.has_scalar_creations()
            || self.owner_transaction.owner_tables().iter().any(|table| {
                self.external.route(*table).is_none_or(|route| {
                    self.external
                        .with_owner(route, |owner, _| Ok(owner.has_scalar_creations()))
                        .unwrap_or(true)
                })
            })
    }

    pub(crate) fn scalar_carry_window(&self) -> Option<u32> {
        let c = self.scalar_carry.as_ref()?;
        // Unavailable entry cannot become a late attachment baseline. The
        // independent refused-scalar subtype keeps its own no-data checks.
        if c.kind.prepared()
            && self
                .unprepared_root_edits
                .iter()
                .any(|(window, _, _)| *window == c.window)
        {
            return None;
        }
        if c.root_acquisition != RootAcquisition::Ready {
            return None;
        }
        // A failed explicit reopen has already closed the old transactions.
        // Its immutable receipt still proves no data was sent; a later attempt
        // must repeat every identity, privilege and version check.
        let session_matches = if self.transaction.is_none() {
            self.owner_transaction.alone()
        } else {
            self.scalar_root_records()
                .is_some_and(|root| c.root.same_records(&root))
                && if c.kind.single() {
                    self.single_root_assets_match(c)
                } else if c.kind.group() && c.assets_detached {
                    self.transaction_asset_counts().is_empty()
                } else {
                    self.assets_match(&c.assets)
                }
                && (if c.kind.group() {
                    self.prepared_group_shape(c, false)
                } else {
                    self.scalar_owner_tables().is_some()
                })
                && c.owners.iter().all(|part| {
                    self.external.route(part.table).is_some_and(|route| {
                        self.external
                            .with_owner(route, |owner, _| {
                                Ok(if c.kind.group() && c.assets_detached {
                                    owner.transaction_asset_counts().is_empty()
                                } else {
                                    owner.assets_match(&part.assets)
                                })
                            })
                            .unwrap_or(false)
                    })
                })
        };
        let mut stamp = c.stamp.clone();
        let permissions = stamp.extend(self)
            && c.owners.iter().all(|part| {
                if self.transaction.is_none() {
                    return part.stamp.proven();
                }
                self.external.route(part.table).is_some_and(|route| {
                    self.external
                        .with_owner(route, |owner, _| Ok(part.stamp.clone().extend(owner)))
                        .unwrap_or(false)
                })
            });
        let explicit = !c.strict()
            || (c.stamp.explicit_identity()
                && c.owners.iter().all(|o| o.stamp.explicit_identity()));
        let assets_allowed =
            c.stamp.assets(&c.assets) && c.owners.iter().all(|o| o.stamp.assets(&o.assets));
        let group = !c.kind.group()
            || ((c.kind == CarryKind::PreparedMixedGroup || c.existing.is_some())
                && c.prepared_membership()
                && if self.transaction.is_some() {
                    self.external.recovery_topology_available()
                } else {
                    c.topology.is_some()
                }
                && !c.assets.is_empty()
                && c.assets.iter().all(asset_recovery::Asset::ownership_known)
                && c.pending_acquisition.is_none()
                && c.owners.iter().all(|o| {
                    (c.kind == CarryKind::PreparedMixedGroup || o.existing.is_some())
                        && o.pending_acquisition.is_none()
                        && (c.kind == CarryKind::PreparedMixedGroup || !o.assets.is_empty())
                        && o.assets.iter().all(asset_recovery::Asset::ownership_known)
                })
                && (self.transaction.is_none() || self.prepared_group_shape(c, false)));
        let single = !c.kind.single()
            || (c.owners.is_empty()
                && c.versions.iter().all(|v| {
                    c.existing
                        .as_ref()
                        .is_some_and(|e| *v == (e.table, e.record, e.version))
                })
                && c.occurrences.is_empty()
                && c.root.single_table().is_some()
                && c.pending_acquisition.is_none()
                && c.assets.iter().all(asset_recovery::Asset::ownership_known)
                && (self.transaction.is_none() || self.single_root_shape().is_some()));
        (c.complete
            && group
            && single
            && permissions
            && explicit
            && assets_allowed
            && self.scalar_session_alone()
            && session_matches
            && c.mark.until(om_data::DataProvider::submission_mark(self)) == Submitted::Nothing)
            .then_some(c.window)
    }

    /// Take a validated carry immediately before closing the old sessions.
    /// Public callers may have corrected a held Root after the refusal. The
    /// receipt still covers its identities, but recovery must keep those latest
    /// accepted values. A retry after closing retains the last captured copy.
    pub(crate) fn take_scalar_carry(&mut self) -> Option<ScalarCarry> {
        let mut carry = self.scalar_carry.take()?;
        if carry.kind.group() && self.transaction.is_some() {
            carry.topology = self.external.recovery_topology();
            carry.complete &= carry.topology.is_some();
        }
        if let Some(root) = self.scalar_root_records() {
            carry.root = root;
        }
        carry.stamp.extend(self);
        for part in &mut carry.owners {
            if let Some(route) = self.external.route(part.table) {
                let _ = self.external.with_owner(route, |owner, _| {
                    part.stamp.extend(owner);
                    if let Some(records) = owner.scalar_records() {
                        part.records = records;
                    }
                    Ok(())
                });
            }
        }
        if carry.kind.prepared() {
            carry.assets_detached = true;
        }
        self.detach_assets();
        for part in &carry.owners {
            if let Some(route) = self.external.route(part.table) {
                let _ = self.external.with_owner(route, |owner, _| {
                    owner.detach_assets();
                    Ok(())
                });
            }
        }
        Some(carry)
    }

    /// Remember only a complete no-data refusal, before rollback parks its rows.
    pub(crate) fn scalar_refused(
        &mut self,
        mark: Option<SubmissionMark>,
        result: &Result<(), DataError>,
    ) {
        let single = self
            .scalar_carry
            .as_ref()
            .or(self.asset_settlement.as_ref())
            .filter(|c| c.kind.prepared())
            .cloned();
        if result.is_err()
            && single.as_ref().is_some_and(|c| {
                c.mark.until(om_data::DataProvider::submission_mark(self)) == Submitted::Nothing
            })
        {
            self.scalar_refusal = None;
            self.scalar_carry = single;
            self.remember_asset_drafts();
            return;
        }
        self.scalar_carry = None;
        self.scalar_refusal = result
            .as_ref()
            .err()
            .filter(|e| {
                !e.commit.is_empty()
                    && e.commit
                        .iter()
                        .all(|p| p.outcome == FileOutcome::NotSubmitted)
            })
            .and(mark)
            .filter(|mark| {
                mark.until(om_data::DataProvider::submission_mark(self)) == Submitted::Nothing
            })
            .and_then(|mark| {
                let versions = self.scalar_versions()?;
                let assets = self.carry_assets().ok()?;
                let stamp = Stamp::read(self);
                let mut owners = BTreeMap::new();
                let mut owner_stamps = BTreeMap::new();
                let mut seen = Vec::<external::TableRoute>::new();
                for &table in self.owner_transaction.owner_tables() {
                    let route = self.external.route(table)?;
                    if seen.iter().any(|r| r.same_file(route)) {
                        continue;
                    }
                    seen.push(route);
                    owner_stamps.insert(
                        table,
                        self.external
                            .with_owner(route, |owner, _| Ok(Stamp::read(owner)))
                            .ok()?,
                    );
                    owners.insert(
                        table,
                        self.external
                            .with_owner(route, |owner, _| owner.carry_assets())
                            .ok()?,
                    );
                }
                Some(Receipt {
                    mark,
                    versions,
                    assets,
                    owners,
                    stamp,
                    owner_stamps,
                })
            });
    }

    // Capture locked versions before rollback removes staged existing edits.
    // The GUI releases temporary pins after staging, so pins alone are insufficient.
    fn scalar_versions(&self) -> Option<Vec<(u64, u64, u64)>> {
        let mut versions = self.scalar_edit_versions()?;
        for &table in self.owner_transaction.owner_tables() {
            let route = self.external.route(table)?;
            let rows = self
                .external
                .with_owner(route, |owner, _| Ok(owner.scalar_edit_versions()))
                .ok()??;
            for (wire, id, version) in rows {
                let table = self.external.client_table(route, wire)?;
                if !versions.contains(&(table, id, version)) {
                    versions.push((table, id, version));
                }
            }
        }
        Some(versions)
    }

    pub(crate) fn remember_scalar_carry(&mut self) {
        let Some(receipt) = self.scalar_refusal.take() else {
            return;
        };
        self.scalar_carry = self.capture_scalar(receipt);
    }

    fn capture_scalar(&self, receipt: Receipt) -> Option<ScalarCarry> {
        if !self.scalar_session_alone() {
            return None;
        }
        let root = self.scalar_root_records()?;
        let identity = self.external.root_identity()?.clone();
        let stamp = receipt.stamp;
        let schema = self.calc.schema.as_ref()?;
        let mut owners = Vec::new();
        let mut seen = Vec::new();
        for table in self.scalar_owner_tables()? {
            let route = self.external.route(table)?;
            if seen
                .iter()
                .any(|r: &external::TableRoute| r.same_file(route))
            {
                continue;
            }
            seen.push(route);
            let identity = self.external.route_identity(route)?;
            let records = self
                .external
                .with_owner(route, |owner, _| {
                    owner.calc_schema_cached()?;
                    Ok(owner.scalar_records())
                })
                .ok()?;
            owners.push(Owner {
                rows: mixed::Rows::default(),
                row: None,
                table,
                routes: self.external.route_tables(route),
                identity,
                stamp: receipt.owner_stamps.get(&table)?.clone(),
                records: records?,
                assets: receipt.owners.get(&table)?.clone(),
                existing: None,
                pending_acquisition: None,
            });
        }
        if owners.is_empty() || (root.is_empty() && owners.iter().all(|o| o.records.is_empty())) {
            return None;
        }
        let mut versions: Vec<_> = receipt.versions;
        let pinned: Option<Vec<_>> = self
            .pinned
            .iter()
            .filter(|((w, table, id), _)| *w == self.window && !root.contains(*table, *id))
            .map(|((_, table, id), v)| Some((*table, *id, (*v)?)))
            .collect();
        versions.extend(pinned?);
        versions.sort_unstable();
        versions.dedup();
        let mut stamp = stamp;
        for &(table, _, _) in &versions {
            match self.external.route(table) {
                Some(route) => {
                    let wire = self.external.with_owner(route, |_, wire| Ok(wire)).ok()?;
                    let part = owners
                        .iter_mut()
                        .find(|o| o.routes.contains(&(table, wire)))?;
                    if let Some(needs) = &mut part.stamp.needs {
                        needs.observe(wire);
                    }
                }
                None => {
                    if let Some(needs) = &mut stamp.needs {
                        needs.observe(table);
                    }
                }
            }
        }
        let occurrences = schema.occurrences().map(|o| o.id).collect();
        Some(ScalarCarry {
            row: None,
            root_acquisition: RootAcquisition::Ready,
            kind: CarryKind::RefusedGroup,
            assets_detached: false,
            detached_counts: BTreeMap::new(),
            pending_acquisition: None,
            window: self.window,
            mark: receipt.mark,
            complete: true,
            stamp,
            identity,
            root,
            existing: None,
            assets: receipt.assets,
            occurrences,
            versions,
            owners,
            intents: Vec::new(),
            topology: None,
        })
    }

    pub(crate) fn owned_asset_counts(&self) -> BTreeMap<(u64, String), u32> {
        let mut counts = self.transaction_asset_counts();
        let parts: Vec<_> = match &self.scalar_carry {
            Some(c)
                if self.transaction.is_none()
                    || c.kind.prepared() && c.assets_detached && c.window == self.window =>
            {
                counts.extend(Self::asset_counts(&c.assets));
                c.owners
                    .iter()
                    .map(|o| (o.table, Some(&o.assets)))
                    .collect()
            }
            _ => self
                .owner_transaction
                .owner_tables()
                .iter()
                .map(|t| (*t, None))
                .collect(),
        };
        for (table, assets) in parts {
            if let Some(route) = self.external.route(table) {
                let owned = match assets {
                    Some(assets) => Self::asset_counts(assets),
                    None => self
                        .external
                        .with_owner(route, |p, _| Ok(p.transaction_asset_counts()))
                        .unwrap_or_default(),
                };
                for ((wire, reference), count) in owned {
                    if let Some(client) = self.external.client_table(route, wire) {
                        counts.insert((client, reference), count);
                    }
                }
            }
        }
        counts
    }

    /// A correction can acquire another member before its next explicit save.
    /// Keep that acknowledged acquisition under the same settlement authority.
    pub(crate) fn add_asset_receipt(&mut self, table: u64, asset: asset_recovery::Asset) {
        let window = self.window;
        for carry in [&mut self.scalar_carry, &mut self.asset_settlement]
            .into_iter()
            .flatten()
            .filter(|c| c.window == window)
        {
            if let Some(part) = carry
                .owners
                .iter_mut()
                .find(|p| p.routes.iter().any(|(client, _)| *client == table))
            {
                asset.add_to(&mut part.assets);
            } else {
                asset.add_to(&mut carry.assets);
            }
        }
    }

    /// A correction can return an acquisition while the immutable creation
    /// receipt remains. Reconcile that ownership, without changing its bytes.
    pub(crate) fn begin_asset_release(
        &mut self,
        table: u64,
        member: &[u8],
        released: i32,
    ) -> Result<(), DataError> {
        self.validate_asset_change()?;
        let window = self.window;
        let mut proofs = Vec::new();
        for carry in [&mut self.scalar_carry, &mut self.asset_settlement]
            .into_iter()
            .flatten()
            .filter(|c| c.window == window)
        {
            carry.asset_mutation(table, member, |a| {
                proofs.push(a.clone());
                Ok(())
            })?;
        }
        if self.custom_asset_receipt(window).is_some()
            && (released <= 0
                || proofs.is_empty()
                || proofs.iter().any(|a| a.ownership().1 < released))
        {
            // Seeing a stored member gives no authority to return a reference.
            return Err(unresolved());
        }
        if proofs.is_empty() {
            return Ok(());
        }
        if let Some(route) = self.external.route(table) {
            self.external
                .with_owner(route, |p, _| p.validate_assets(&proofs))?;
        } else {
            self.validate_assets(&proofs)?;
        }
        for carry in [&mut self.scalar_carry, &mut self.asset_settlement]
            .into_iter()
            .flatten()
            .filter(|c| c.window == window)
        {
            carry.asset_mutation(table, member, |a| a.release_started())?;
        }
        Ok(())
    }

    pub(crate) fn acknowledge_asset_release(&mut self, table: u64, member: &[u8], released: i32) {
        let window = self.window;
        for carry in [&mut self.scalar_carry, &mut self.asset_settlement]
            .into_iter()
            .flatten()
            .filter(|c| c.window == window)
        {
            let _ = carry.asset_mutation(table, member, |a| {
                a.release_acknowledged(released);
                Ok(())
            });
        }
    }

    pub(crate) fn remember_asset_drafts(&mut self) {
        if self.transaction_kind() != Some(transaction::TransactionKind::Record) {
            return;
        }
        let settlement = self.scalar_carry.is_none();
        let source = if settlement {
            &mut self.asset_settlement
        } else {
            &mut self.scalar_carry
        };
        if !source
            .as_ref()
            .is_some_and(|c| c.window == self.window && c.has_assets() && c.strict())
        {
            return;
        }
        let Some(mut carry) = source.take() else {
            return;
        };
        if carry.kind.group() {
            carry.topology = self.external.recovery_topology();
            carry.complete &= carry.topology.is_some();
            // Save's temporary rollback mark is still present here. Retention
            // may cross that mark only under the original no-submission proof;
            // reconnect availability still requires the unmarked live shape.
            carry.complete &= self.prepared_group_shape(&carry, true);
        }
        if carry.kind.single() {
            // Closing must not turn a live shape/provenance failure into an
            // eligible empty-session receipt on the next reconnect attempt.
            carry.complete &=
                self.single_root_live_shape().is_some() && self.single_root_assets_match(&carry);
        }
        if let Some(root) = self.asset_draft_records() {
            carry.root = root;
        } else {
            carry.complete = false;
        }
        carry.complete &= self.scalar_footprint().is_some();
        carry.stamp.extend(self);
        for part in &mut carry.owners {
            if let Some(route) = self.external.route(part.table) {
                let _ = self.external.with_owner(route, |owner, _| {
                    if let Some(records) = owner.asset_draft_records() {
                        part.records = records;
                    } else {
                        carry.complete = false;
                    }
                    if carry.kind.group() {
                        if let Some(pending) = owner
                            .transaction
                            .as_ref()
                            .and_then(|t| t.single_root_start.as_ref())
                            .and_then(|s| s.upload.clone())
                        {
                            part.pending_acquisition = Some(pending);
                        }
                    }
                    carry.complete &= owner.scalar_footprint().is_some();
                    part.stamp.extend(owner);
                    Ok(())
                });
            }
        }
        if let Some(versions) = self.scalar_versions() {
            carry.versions.extend(versions);
            carry.versions.sort_unstable();
            carry.versions.dedup();
        } else {
            carry.complete = false;
        }
        if settlement {
            self.asset_settlement = Some(carry);
        } else {
            self.scalar_carry = Some(carry);
        }
    }

    /// Closing a session cannot settle an uncertain release or lose the only
    /// remaining bytes. Explicit reconnect/revert owns the retained receipt.
    pub(crate) fn park_asset_receipt(&mut self) {
        self.remember_asset_drafts();
        if self.scalar_carry.is_none() {
            self.scalar_carry = self.asset_settlement.take();
        }
        if let Some(mut carry) = self.scalar_carry.take() {
            self.detach_scalar_assets(&mut carry);
            self.scalar_carry = Some(carry);
        }
    }

    /// The receipt is the sole cleanup authority, including when its owning
    /// record session is parked behind another window or a script transaction.
    fn detach_scalar_assets(&mut self, carry: &mut ScalarCarry) {
        if carry.kind.group() {
            carry.assets_detached = true;
            // Detach even an ACKed acquisition whose fingerprint read failed.
            // Preserve its raw ledger separately; it cannot authorize release.
            for table in std::iter::once(None).chain(carry.owners.iter().map(|p| Some(p.table))) {
                let mut counts = self.detach_attachment_counts(carry.window, table);
                counts.retain(|_, count| *count > 0);
                if !counts.is_empty() {
                    let assets = match table {
                        None => carry.assets.as_slice(),
                        Some(table) => carry
                            .owners
                            .iter()
                            .find(|p| p.table == table)
                            .map(|p| p.assets.as_slice())
                            .unwrap_or_default(),
                    };
                    let known: BTreeMap<_, _> = assets
                        .iter()
                        .map(|a| a.ownership())
                        .filter(|(_, n)| *n > 0)
                        .collect();
                    if counts != known {
                        carry.complete = false;
                        let retained = carry.detached_counts.entry(table).or_default();
                        for (key, count) in counts {
                            *retained.entry(key).or_default() += count;
                        }
                    }
                }
            }
            return;
        }
        if carry.kind.single() {
            // A missing upload fingerprint may still have an ACKed acquisition.
            // Its retained bytes/ACK are unresolved, never cleanup authority.
            carry.assets_detached = true;
            self.detach_single_root_assets(carry.window);
            return;
        }
        self.detach_parked_assets(carry.window, None, &carry.assets);
        for part in &carry.owners {
            self.detach_parked_assets(carry.window, Some(part.table), &part.assets);
        }
        if carry.window == self.window
            && self.transaction_kind() == Some(transaction::TransactionKind::Record)
        {
            self.detach_carried_assets(&carry.assets);
            for part in &carry.owners {
                if let Some(route) = self.external.route(part.table) {
                    let _ = self.external.with_owner(route, |owner, _| {
                        owner.detach_carried_assets(&part.assets);
                        Ok(())
                    });
                }
            }
        }
    }

    pub(crate) fn discard_scalar_assets(&mut self) -> Result<(), DataError> {
        self.discard_window_assets(self.window)
    }

    pub(crate) fn discard_window_assets(&mut self, window: u32) -> Result<(), DataError> {
        if window == self.window {
            self.remember_asset_drafts();
        }
        if !self
            .scalar_carry
            .as_ref()
            .or(self.asset_settlement.as_ref())
            .is_some_and(|c| c.window == window)
        {
            return Ok(());
        }
        let Some(mut carry) = self
            .scalar_carry
            .take()
            .or_else(|| self.asset_settlement.take())
        else {
            return Ok(());
        };
        // Ownership moves into this receipt before any release attempt. A
        // failed validation or reply must not leave a second cleanup authority.
        self.detach_scalar_assets(&mut carry);
        let result = (|| {
            if !carry.has_assets() {
                return Ok(());
            }
            if !carry.stamp.matches(self) || self.external.root_identity() != Some(&carry.identity)
            {
                return Err(unresolved());
            }
            // Mixed/custom cleanup uses the same all-owner proof as reconnect.
            // A failed check cannot fall through to transaction cleanup.
            if carry.strict() {
                self.validate_carry_group(&carry)?;
            }
            // Validate all ownership before the first decrement.
            self.validate_assets(&carry.assets)?;
            for part in &carry.owners {
                let route = self.external.route(part.table).ok_or_else(unresolved)?;
                if self.external.route_identity(route).as_ref() != Some(&part.identity) {
                    return Err(unresolved());
                }
                self.external.with_owner(route, |owner, _| {
                    if !part.stamp.matches(owner) {
                        return Err(unresolved());
                    }
                    for row in part.rows.members.values() {
                        if let mixed::RowReceipt::Existing(original) = row {
                            owner.validate_attachment_original(original)?;
                        }
                    }
                    owner.validate_assets(&part.assets)
                })?;
            }
            self.release_carried_assets(&mut carry.assets)?;
            for part in &mut carry.owners {
                let route = self.external.route(part.table).ok_or_else(unresolved)?;
                self.external.with_owner(route, |owner, _| {
                    owner.release_carried_assets(&mut part.assets)
                })?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.scalar_carry = Some(carry);
        }
        result
    }

    // All reads and permission proofs precede the first reservation, count
    // adjustment or ownership transfer, including cleanup of a parked window.
    fn validate_carry_group(&mut self, carry: &ScalarCarry) -> Result<(), DataError> {
        let result = (|| {
            if let Some(pending) = &carry.pending_acquisition {
                return Err(DataError::with_code(
                    3,
                    if pending.acknowledged.is_some() {
                        "Unresolved: the acknowledged container acquisition could not be verified. Its data is kept."
                    } else {
                        "Unresolved: the container acquisition was not acknowledged. Its data is kept."
                    },
                ));
            }
            if carry.owners.iter().any(|o| o.pending_acquisition.is_some()) {
                return Err(DataError::with_code(3, "Unresolved: a related attachment upload has no complete acknowledgement. Its bytes are kept and will not be sent again."));
            }
            if !carry.complete
                || carry.kind.group()
                    && ((carry.kind != CarryKind::PreparedMixedGroup && carry.existing.is_none())
                        || !carry.prepared_membership()
                        || carry.owners.iter().any(|o| {
                            (carry.kind != CarryKind::PreparedMixedGroup && o.existing.is_none())
                                || o.pending_acquisition.is_some()
                        }))
                || carry.kind.single()
                    && (!carry.owners.is_empty()
                        || carry.versions.iter().any(|v| {
                            carry
                                .existing
                                .as_ref()
                                .is_none_or(|e| *v != (e.table, e.record, e.version))
                        })
                        || !carry.occurrences.is_empty()
                        || carry.root.single_table().is_none())
            {
                return Err(unresolved());
            }
            self.calc_schema_cached()?;
            if carry.strict()
                && carry
                    .mark
                    .until(om_data::DataProvider::submission_mark(self))
                    != Submitted::Nothing
            {
                return Err(unresolved());
            }
            if !carry.stamp.matches_group(self, carry.strict())
                || self.external.root_identity() != Some(&carry.identity)
            {
                return Err(unresolved());
            }
            if !carry.stamp.assets(&carry.assets) {
                return Err(unresolved());
            }
            carry.root.validate(self)?;
            if let Some(original) = &carry.existing {
                self.validate_attachment_original(original)?;
            }
            self.validate_assets(&carry.assets)?;
            for part in &carry.owners {
                let route = self.external.route(part.table).ok_or_else(unresolved)?;
                if self.external.route_tables(route) != part.routes
                    || self.external.route_identity(route).as_ref() != Some(&part.identity)
                {
                    return Err(unresolved());
                }
                self.external.with_owner(route, |owner, _| {
                    owner.calc_schema_cached()?;
                    if !part.stamp.matches_group(owner, carry.strict())
                        || !part.stamp.assets(&part.assets)
                    {
                        return Err(unresolved());
                    }
                    owner.require_writable()?;
                    part.records.validate(owner)?;
                    if let Some(original) = &part.existing {
                        owner.validate_attachment_original(original)?;
                    }
                    for row in part.rows.members.values() {
                        if let mixed::RowReceipt::Existing(original) = row {
                            owner.validate_attachment_original(original)?;
                        }
                    }
                    owner.validate_assets(&part.assets)
                })?;
            }
            // Existing drafts must still name the host versions the user edited.
            for &(table, id, version) in &carry.versions {
                let route = self.external.route(table);
                let check = |p: &mut NativeProvider, wire| {
                    // The original footprint distinguishes actual edits from
                    // observation pins; a read-only pin does not need Edit.
                    let stamp = if route.is_some() {
                        &carry
                            .owners
                            .iter()
                            .find(|o| o.routes.contains(&(table, wire)))
                            .ok_or_else(unresolved)?
                            .stamp
                    } else {
                        &carry.stamp
                    };
                    if stamp.full
                        || stamp
                            .needs
                            .as_ref()
                            .and_then(|n| n.tables.get(&wire))
                            .is_some_and(|n| n.edit)
                    {
                        p.require_action(wire, RecordAction::Edit)?;
                    }
                    let row = p.read_record(wire, id)?.ok_or_else(unresolved)?;
                    if records::mod_count(&records::stored_record(id, &row))? != version {
                        return Err(unresolved());
                    }
                    Ok(())
                };
                match route {
                    Some(route) => self.external.with_owner(route, check)?,
                    None => check(self, table)?,
                }
            }
            Ok(())
        })();
        result.map_err(|_| carry.unresolved())
    }

    fn custom_asset_receipt(&self, window: u32) -> Option<ScalarCarry> {
        self.scalar_carry
            .as_ref()
            .or(self.asset_settlement.as_ref())
            .filter(|c| c.window == window && c.has_assets() && c.strict())
            .cloned()
    }

    fn single_root_assets_match(&self, carry: &ScalarCarry) -> bool {
        self.cached_root_assets(carry.assets_detached)
            .is_ok_and(|assets| Self::asset_counts(&assets) == Self::asset_counts(&carry.assets))
    }

    // Ordinary live work may grow beyond this recovery shape. Its already
    // owned acquisitions stay with the live transaction; they never become
    // permission to reconnect a larger group without that group's receipt.
    pub(crate) fn leave_single_root_recovery(&mut self) -> Result<(), DataError> {
        let Some(carry) = self
            .scalar_carry
            .as_ref()
            .filter(|c| c.kind.single())
            .cloned()
        else {
            return Ok(());
        };
        if carry.assets_detached
            || self.transaction.is_none()
            || !self.owner_transaction.alone()
            || !self.assets_match(&carry.assets)
        {
            return Err(unresolved());
        }
        self.validate_carry_group(&carry)?;
        self.scalar_carry = None;
        if let Some(t) = &mut self.transaction {
            t.single_root_start = None;
        }
        Ok(())
    }

    pub(crate) fn validate_asset_change(&mut self) -> Result<(), DataError> {
        if let Some(carry) = self.custom_asset_receipt(self.window) {
            if carry.kind.prepared() && carry.assets_detached {
                return Err(unresolved());
            }
            self.validate_carry_group(&carry)?;
        }
        Ok(())
    }

    // An acquisition added after refusal still needs the ORIGINAL owner's
    // rules. An ACK extends ownership, never permission or record provenance.
    pub(crate) fn check_asset_acquisition(
        &mut self,
        table: u64,
        field: Option<u64>,
    ) -> Result<(), DataError> {
        let Some(carry) = self.custom_asset_receipt(self.window) else {
            return Ok(());
        };
        if carry.kind.group() && field.is_none() {
            return Err(unresolved());
        }
        if carry.kind.prepared() && carry.assets_detached {
            return Err(unresolved());
        }
        self.validate_carry_group(&carry)?;
        if carry.kind.single() && (field.is_none() || carry.root.single_table() != Some(table)) {
            return self.leave_single_root_recovery();
        }
        let (stamp, wire) = match self.external.route(table) {
            Some(route) => {
                let wire = self.external.with_owner(route, |_, wire| Ok(wire))?;
                let owner = carry
                    .owners
                    .iter()
                    .find(|o| o.routes.contains(&(table, wire)))
                    .ok_or_else(unresolved)?;
                (&owner.stamp, wire)
            }
            None => (&carry.stamp, table),
        };
        if !stamp.library(wire, true)
            || field.is_some_and(|field| {
                !stamp.full
                    && stamp
                        .access
                        .as_ref()
                        .is_none_or(|a| !a.modifies(&key_bytes(wire), &key_bytes(field)))
            })
        {
            return Err(unresolved());
        }
        self.extend_mixed_acquisition(table, field)?;
        Ok(())
    }

    pub(crate) fn check_asset_write(
        &mut self,
        write: &om_data::ScriptWrite,
        table: u64,
    ) -> Result<(), DataError> {
        if self.prepare_mixed_write(write, table)? {
            return Ok(());
        }
        let Some(carry) = self.custom_asset_receipt(self.window) else {
            return Ok(());
        };
        if carry.kind.single()
            && !write
                .record
                .is_some_and(|id| u64::try_from(id).is_ok_and(|id| carry.root.contains(table, id)))
        {
            return self.leave_single_root_recovery();
        }
        self.validate_carry_group(&carry)?;
        let (mut stamp, wire, records) = match self.external.route(table) {
            Some(route) => {
                let wire = self.external.with_owner(route, |_, wire| Ok(wire))?;
                let owner = carry
                    .owners
                    .iter()
                    .find(|o| o.routes.contains(&(table, wire)))
                    .ok_or_else(unresolved)?;
                (owner.stamp.clone(), wire, &owner.records)
            }
            None => (carry.stamp.clone(), table, &carry.root),
        };
        let definition = stamp.catalog.table(wire).ok_or_else(unresolved)?;
        let mut fields = write
            .cells
            .keys()
            .map(|cell| {
                definition
                    .field(*cell)
                    .or_else(|| definition.field(*cell >> 8))
                    .map(|f| f.id)
                    .ok_or_else(unresolved)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        for name in write.fields.keys() {
            fields.insert(definition.field_by_name(name).ok_or_else(unresolved)?.id);
        }
        let create = carry.existing.is_none()
            && write
                .record
                .is_none_or(|id| u64::try_from(id).is_ok_and(|id| records.contains(wire, id)));
        let layouts = if self.external.route(table).is_none() {
            [write.layout].into_iter().collect()
        } else {
            BTreeSet::new()
        };
        stamp
            .needs
            .as_mut()
            .ok_or_else(unresolved)?
            .write(wire, create, fields, &layouts);
        if !stamp.proven() {
            return Err(unresolved());
        }
        Ok(())
    }

    /// Explicit Reconnect only. Validate every participant before reserving any id.
    pub(crate) fn recover_scalar_carry(&mut self, carry: &ScalarCarry) -> Result<(), DataError> {
        self.calc_schema_cached()?;
        if carry.kind.group() {
            let topology = carry.topology.as_ref().ok_or_else(unresolved)?;
            // Passive imports consume published ids too. Repeat the original
            // read groups, never participant-only or restamped reconstruction.
            for demand in topology.demands() {
                self.grow_related(demand.iter().copied())?;
            }
            if !topology.matches(&self.external) {
                return Err(unresolved());
            }
        } else {
            self.grow_related(carry.occurrences.iter().copied())?;
        }
        if carry.window != self.window || carry.root_acquisition != RootAcquisition::Ready {
            return Err(unresolved());
        }
        self.validate_carry_group(carry)?;
        // No auto-entry, clock, UUID or serial generation: only fresh wire ids.
        om_data::DataProvider::begin_record_commit(self)?;
        if carry.kind.prepared() {
            if let Some(t) = &mut self.transaction {
                t.single_root_start = Some(RootStart {
                    rows: mixed::Rows::default(),
                    row: carry.row.clone(),
                    participant: false,
                    upload: None,
                    mark: carry.mark,
                    stamp: carry.stamp.clone(),
                    identity: carry.identity.clone(),
                    existing: carry.existing.clone(),
                });
            }
        }
        let mut remapped = Vec::new();
        let held = (|| {
            for (table, old, new) in self.restore_scalar_records(&carry.root, false)? {
                remapped.push(om_data::RecreatedRecord {
                    window: carry.window,
                    table,
                    old,
                    new: Ok(new),
                });
            }
            for part in &carry.owners {
                let route = self.external.route(part.table).ok_or_else(unresolved)?;
                self.join_owner_transaction(part.table, route)?;
                let rows = self.external.with_owner(route, |owner, _| {
                    if carry.kind.group() {
                        let t = owner.transaction.as_mut().ok_or_else(unresolved)?;
                        t.single_root_start = Some(RootStart {
                            rows: part.rows.clone(),
                            row: part.row.clone(),
                            participant: true,
                            upload: None,
                            mark: carry.mark,
                            stamp: part.stamp.clone(),
                            identity: part.identity.clone(),
                            existing: part.existing.clone(),
                        });
                    }
                    owner.restore_scalar_records(&part.records, !carry.kind.group())
                })?;
                if carry.kind == CarryKind::PreparedExistingGroup {
                    let original = part.existing.as_ref().ok_or_else(unresolved)?;
                    let client = self
                        .external
                        .client_table(route, original.table)
                        .ok_or_else(unresolved)?;
                    self.restored_attachment_record(client, original.table, original.record);
                }
                for (wire, old, new) in rows {
                    let table = self
                        .external
                        .client_table(route, wire)
                        .ok_or_else(unresolved)?;
                    remapped.push(om_data::RecreatedRecord {
                        window: carry.window,
                        table,
                        old,
                        new: Ok(new),
                    });
                }
            }
            // The receipt is already validated. Update only its newly mapped
            // record identities; recapturing optional observation pins here
            // could silently drop the asset settlement authority.
            let mut settlement = carry.clone();
            settlement.root = self.scalar_root_records().ok_or_else(unresolved)?;
            if carry.kind == CarryKind::PreparedMixedGroup {
                settlement.row = self.recovery_row();
            }
            for part in &mut settlement.owners {
                let route = self.external.route(part.table).ok_or_else(unresolved)?;
                part.records = self.external.with_owner(route, |owner, _| {
                    owner.scalar_records().ok_or_else(unresolved)
                })?;
                if carry.kind == CarryKind::PreparedMixedGroup {
                    part.rows = self.external.with_owner(route, |owner, _| {
                        Ok(owner
                            .transaction
                            .as_ref()
                            .and_then(|t| t.single_root_start.as_ref())
                            .ok_or_else(unresolved)?
                            .rows
                            .clone())
                    })?;
                    self.restore_mixed_metadata(part)?;
                }
            }
            Ok(settlement)
        })();
        let mut settlement = match held {
            Ok(settlement) => settlement,
            Err(error) => {
                let _ = om_data::DataProvider::revert_transaction(self);
                return Err(error);
            }
        };
        // The whole group now owns fresh reservations. Only now transfer its
        // acknowledged acquisitions; failed partial recreation releases none.
        self.attach_assets(&carry.assets);
        if carry.kind.prepared() {
            settlement.assets_detached = false;
            if let Some(t) = &mut self.transaction {
                t.before_record = Some(carry.mark);
                t.single_root_start = Some(RootStart {
                    rows: mixed::Rows::default(),
                    row: settlement.row.clone(),
                    participant: false,
                    upload: None,
                    mark: carry.mark,
                    stamp: carry.stamp.clone(),
                    identity: carry.identity.clone(),
                    existing: carry.existing.clone(),
                });
                t.root_acquisitions = t.containers.clone();
                t.upload_attempted = true;
            }
        }
        for part in &carry.owners {
            let route = self.external.route(part.table).ok_or_else(unresolved)?;
            self.external.with_owner(route, |owner, _| {
                owner.attach_assets(&part.assets);
                Ok(())
            })?;
        }
        for &(table, id, version) in &carry.versions {
            self.pinned.insert((carry.window, table, id), Some(version));
        }
        for record in &remapped {
            if carry.root.contains(record.table, record.old) {
                if let Some(found) = self.found.get_mut(&record.table) {
                    found.renumber(record.old, *record.new.as_ref().map_err(Clone::clone)?);
                }
            }
        }
        self.recreated.extend(remapped);
        if carry.kind.prepared() {
            self.scalar_carry = Some(settlement);
        } else if carry.has_assets() {
            self.asset_settlement = Some(settlement);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_upload_error_retains_payload_without_inventing_acquisition() {
        let provider = NativeProvider::new(NativeConfig {
            client_cert_pem: Vec::new(),
            client_key_pem: Vec::new(),
            additional_ca_pem: Vec::new(),
            callback_host: String::new(),
            machine: MachineInfo::default(),
        });
        let mut owner = Owner {
            rows: mixed::Rows::default(),
            row: None,
            table: 7,
            routes: vec![(7, 1)],
            identity: om_data::file_refs::FileIdentity::hosted("127.0.0.1", 1, "Related"),
            stamp: Stamp::read(&provider),
            records: transaction::ScalarRecords::default(),
            assets: Vec::new(),
            existing: None,
            pending_acquisition: None,
        };
        let bytes = b"exact pre-wire request, including unknown streams\0\xff".to_vec();
        assert!(!owner.collect_upload(
            Some(PendingAcquisition {
                kind: AcquisitionKind::Upload,
                target: None,
                bytes: bytes.clone(),
                acknowledged: None
            }),
            None,
            Err(DataError::new("upload reply lost"))
        ));
        let retained = owner.pending_acquisition.as_ref().unwrap();
        assert_eq!(retained.bytes, bytes);
        assert!(retained.acknowledged.is_none());
        assert!(
            owner.assets.is_empty(),
            "unknown reply creates no release authority"
        );
        assert!(!owner.stamp.proven(), "unknown metadata is not promoted");
    }

    #[test]
    fn a_closed_scalar_carry_survives_failed_reopen_until_its_window_reverts() {
        let mut provider = NativeProvider::new(NativeConfig {
            client_cert_pem: Vec::new(),
            client_key_pem: Vec::new(),
            additional_ca_pem: Vec::new(),
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        });
        provider.use_window(7);
        // State retained after the original sessions have closed, before a
        // transient failed connection can authenticate and revalidate them.
        provider.scalar_carry = Some(ScalarCarry {
            row: None,
            root_acquisition: RootAcquisition::Ready,
            kind: CarryKind::RefusedGroup,
            assets_detached: false,
            detached_counts: BTreeMap::new(),
            pending_acquisition: None,
            window: 7,
            mark: provider.submission_mark().unwrap(),
            complete: true,
            stamp: Stamp {
                creation: Some(1),
                account: "test".into(),
                full: true,
                writable: true,
                ..Stamp::read(&provider)
            },
            identity: om_data::file_refs::FileIdentity::hosted("127.0.0.1", 1, "Root"),
            root: transaction::ScalarRecords::default(),
            existing: None,
            assets: Vec::new(),
            occurrences: Vec::new(),
            versions: vec![(0x8001, 3, 1)],
            owners: Vec::new(),
            intents: Vec::new(),
            topology: None,
        });
        for _ in 0..2 {
            let mut attempted = false;
            let error = provider
                .reopen_with(|p| {
                    attempted = true;
                    p.close();
                    Err(DataError::new("connection unavailable"))
                })
                .unwrap_err();
            assert!(
                attempted,
                "the explicit retry reaches the connection attempt"
            );
            assert_eq!(error.message, "connection unavailable");
            assert_eq!(provider.carried_record_sessions(), vec![7]);
            assert_eq!(
                provider.scalar_carry.as_ref().unwrap().versions,
                vec![(0x8001, 3, 1)]
            );
        }
        provider.use_window(8);
        provider.revert_record_session().unwrap();
        assert_eq!(provider.carried_record_sessions(), vec![7]);
        provider.use_window(7);
        provider.revert_record_session().unwrap();
        assert!(provider.scalar_carry.is_none());
        assert!(provider.carried_record_sessions().is_empty());
    }
}
