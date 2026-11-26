//! The window a script works on, on the worker: its layout, mode, found-set
//! position, the current record and its unsaved values, Find mode's
//! requests. Record and found-set changes go straight to the provider.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use om_data::found::{code, FindRequest, FoundSet, FoundSetChange, SortField};
use om_data::runtime::Mode;
use om_data::{DataError, DataProvider, Field, FieldChanges, Record, ScriptWrite};

use super::host::transaction::{missing_related_parent, Staged};
use super::{ScrollRequest, WindowState, WindowUpdate};

/// Values by cell: a field's id for its first repetition, `F * 256 + n`
/// for repetition n; stored text, `None` empty.
pub(crate) type Cells = BTreeMap<u64, Option<String>>;

/// A related record a script or the user changed, or a script is
/// creating, through the current record (a portal row, or a related
/// field's first record): it is written when the current record is
/// committed, as FileMaker commits a record with its related records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelatedDraft {
    /// The draft's identity while it lives (`PortalIdentity::New`): its
    /// place in `Uncommitted::related` changes as other drafts go.
    pub key: DraftKey,
    /// The occurrence the record was reached through, and its base table.
    pub occurrence: u64,
    pub table: u64,
    /// `None`: a record to create (the relationship allows creation; its
    /// match fields are among `cells`).
    pub record: Option<u64>,
    pub cells: Cells,
    /// The cells of `cells` the user typed in Browse: checked by a user's
    /// rules as a value is entered (`ScriptWrite::typed`).
    pub typed: BTreeSet<u64>,
    /// For a record to create: the id the window's record session holds
    /// it under after a commit that wrote it was refused before any record
    /// data (`DataProvider::related_record_reserved`, #1095). The next
    /// write names it, so the record keeps the id, serial numbers and
    /// creation values it was given, and nothing is reserved again.
    pub reserved: Option<i64>,
    /// The cells of `cells` a commit filled from the window's record, a
    /// begun row's match fields (`match_begun_records`): derived, filled
    /// again at every save, so a commit that sent nothing leaves nothing
    /// in them a reconnect cannot keep (#152).
    pub matched_cells: BTreeSet<u64>,
    /// The record's modification count as the window showed it when the
    /// user began typing into it: its commit is refused (306) when another
    /// client changed it since (`DataProvider::retain_related_record`).
    pub version: Option<u64>,
    /// The window's id of the record's row when the record is one to
    /// create shown as a row of its portal (`document::RelatedRow::New`):
    /// one the user began in a creation row, or one a step created
    /// (adopted, `matched`); `None` for other records.
    pub begun: Option<u64>,
    /// A step created the record through the relationship and set its
    /// match fields then (`match_cells`): the save leaves them as they are,
    /// a script's later Set Field of one too. The user's begun records
    /// take theirs at the save (`match_begun_records`).
    pub matched: bool,
    /// The stored parent (base table and record) of a script-created
    /// draft reached through an intermediate occurrence.
    pub creation_parent: Option<(u64, u64)>,
    /// Delete Portal Row deleted the record.
    pub deleted: bool,
    /// The cells of `typed` whose field object's OnObjectSave already ran,
    /// with the value each had then: leaving the field again does not save
    /// it again while it still holds that value (#1040). Any later change
    /// of it, typed or chosen, saves it again.
    pub saved: BTreeMap<u64, Option<String>>,
    /// For a record to create: what its own file's rules enter, computed
    /// before its commit (#1047, [`PreparedDraft`]).
    pub prepared: Option<PreparedDraft>,
}

/// A record to create as its file's provider previewed it before the
/// commit (`DataProvider::preview_new_record`, #1047): the auto-entered
/// values and the stored calculations its own cells give, which its
/// commit's OnObjectSave script and Browse show under what was typed, and
/// which the commit stores (`ScriptWrite::prepared` names the preview).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedDraft {
    pub id: om_data::PreparedId,
    /// The cells the rules enter (`NewRecordPreview::values`).
    pub values: Cells,
    /// The draft's own cells the preview was computed over, and the
    /// window's other unsaved records it read them over (#1063; `None`:
    /// none, the provider was not given them): it is computed again only
    /// when either changed.
    pub over: Cells,
    pub context: Option<Vec<om_data::PendingRecord>>,
}

/// A related draft's identity ([`RelatedDraft::key`]), unique in the
/// process and kept by its copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DraftKey(u64);

impl DraftKey {
    /// A key no other draft has.
    pub fn fresh() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

impl RelatedDraft {
    /// The record's cells as its commit would begin them: its own over
    /// what the preview of its rules entered (#1047).
    pub fn previewed_cells(&self) -> Cells {
        let mut cells = self
            .prepared
            .as_ref()
            .map(|prepared| prepared.values.clone())
            .unwrap_or_default();
        cells.extend(self.cells.clone());
        cells
    }
}

/// What scripts left on the current record that no commit has kept yet,
/// besides its drafts by name. It belongs to the record, not to a script:
/// the UI keeps it with the window's record between scripts
/// (`app/windows.rs`), so the user's save commits it, the user's revert
/// drops it and the next script starts from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Uncommitted {
    /// The cells scripts set, repetitions too (first repetitions are also
    /// drafts by name, which the UI shows).
    pub cells: Cells,
    /// Related records changed, created or deleted through the record.
    pub related: Vec<RelatedDraft>,
    /// New Record made the record: Revert Record removes it. The fields
    /// that held a value when it was made (its auto-entered ones).
    pub new: bool,
    pub entered: Vec<String>,
    /// The record is held in the window's record session
    /// (`DataProvider::begin_record_commit`), not on the host: the commit
    /// writes it, Revert releases it (#892).
    pub session: bool,
    /// The provider retained an all-file no-data receipt and issued scalar values.
    pub related_recoverable: bool,
    /// The sole existing-record attachment field prepared before acquisition.
    pub attachment_field: Option<u64>,
    /// Existing Related attachment targets prepared before acquisition.
    pub related_attachments: BTreeMap<(u64, u64), u64>,
    /// New Record wrote the record at once (a provider without
    /// transactions): it is on the host, where another client may have
    /// changed it since, so Revert drops only the window's changes and
    /// keeps it.
    pub saved: bool,
    /// The new record's place in the found set when it was made, which
    /// its auto-entered `Get ( RecordNumber )` was given: its later
    /// writes before its commit give it again, as the value is entered
    /// when the record is made (Duplicate Record, #526). `None` when it
    /// is not known (New Record, #298).
    pub record_number: Option<u64>,
}

impl Uncommitted {
    /// The place in [`Self::related`] of the draft `key` names, while it
    /// is there.
    pub fn draft_index(&self, key: DraftKey) -> Option<usize> {
        self.related.iter().position(|draft| draft.key == key)
    }

    pub fn is_empty(&self) -> bool {
        // A prepared scalar-only entry already owns a record session. A
        // dialog/trigger snapshot must not orphan it before cells are staged.
        !self.session
            && self.cells.is_empty()
            && self.related.is_empty()
            && !self.new
            && self.attachment_field.is_none()
            && self.related_attachments.is_empty()
    }

    /// Whether Delete Portal Row deleted record `record` of base table
    /// `table` (through any occurrence of it).
    pub fn deletes(&self, table: u64, record: u64) -> bool {
        self.related
            .iter()
            .any(|d| d.deleted && d.table == table && d.record == Some(record))
    }
}

/// The current record's unsaved changes ([`Window::pending`]).
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    layout: Option<u32>,
    record: Option<i64>,
    drafts: FieldChanges,
    uncommitted: Uncommitted,
}

impl Pending {
    /// Revert Record settled this record's drafts. Keep the window in
    /// rollback cleanup without restoring what the step discarded.
    pub fn discard_drafts_for(&mut self, layout: Option<u32>, record: Option<i64>) {
        if self.layout == layout && self.record == record {
            self.drafts.clear();
            self.uncommitted = Uncommitted::default();
        }
    }
}

/// A new record [`Window::make_new_record`] made, for
/// [`Window::adopt_new_record`].
pub(crate) struct NewRecord {
    record: Record,
    /// Held in the window's record session.
    session: bool,
    /// Made at once on the host.
    saved: bool,
    /// Its place in the found set when it was made, if known
    /// ([`Uncommitted::record_number`]).
    record_number: Option<u64>,
}

/// What [`Window::commit`] writes with, as its caller gave it.
struct CommitWrite<'a> {
    cell_of: &'a dyn Fn(&str) -> Option<u64>,
    skip_validation: bool,
    privilege_set: Option<String>,
    record_numbers: &'a BTreeMap<usize, u64>,
    transaction: Option<&'a mut Staged>,
}

/// A record a commit wrote (OnWindowTransaction's parameter).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Committed {
    /// The base table; `None` for the layout's.
    pub table: Option<u64>,
    pub record: i64,
    pub new: bool,
    pub deleted: bool,
}

/// One page holds the current record.
const ONE: u32 = 1;

/// A table's last sort order in a window: by names, and the stored order it
/// came from (`Window::last_sort_spec`).
type TableSort = (Vec<SortField>, Option<LastSortSpec>);

/// A stored sort order as last accepted ([`Window::last_sort_spec`]).
#[derive(Clone)]
pub(crate) struct LastSortSpec {
    /// The base table it sorted.
    pub table: Option<u64>,
    /// The layout occurrence it was accepted on. Its keys of that
    /// occurrence are the layout's own fields on any layout of the table;
    /// a key of another occurrence, a self join's included, stays related.
    pub occurrence: Option<u64>,
    pub spec: om_data::specs::SortSpec,
}

#[derive(Clone, Default)]
pub(crate) struct Window {
    pub state: WindowState,
    /// The current record and its table's fields, as last read.
    record: Option<(Vec<Field>, Record)>,
    /// The found set's size, once read.
    pub found: Option<u64>,
    pub sorted: bool,
    /// The found set a find, omit or sort left, for the UI.
    found_set: Option<FoundSet>,
    /// Freeze Window: the UI is not told until Refresh Window or the end.
    pub frozen: bool,
    dirty: bool,
    data_changed: bool,
    refreshed: bool,
    objects: Vec<super::ObjectRefresh>,
    record_stale: bool,
    /// The window left a new record its record session held without
    /// committing or reverting it (a found-set change, say): the session
    /// is settled before the next read ([`Self::settle_stray_session`]).
    stray_session: bool,
    /// The new related records the commit under way wrote, by their place
    /// in `Uncommitted::related`, with the ids they were given: kept by a
    /// failed commit's [`Self::restore`] (`RelatedDraft::reserved`).
    reserved_made: Vec<(usize, i64)>,
    scroll: Vec<ScrollRequest>,
    /// The tab panels steps brought to the front since the last update
    /// (`WindowUpdate::fronted`).
    pub fronted: Vec<(u64, u64, u64)>,
    /// The layout's base table, and the record each table showed last
    /// (FileMaker keeps a table's current record across its layouts).
    pub table: Option<u64>,
    table_index: HashMap<u64, u64>,
    /// The layout's table is one the window had not shown
    /// ([`Self::start_on_saved_record`]).
    first_visit: bool,
    pub last_find: Vec<FindRequest>,
    pub last_sort: Vec<SortField>,
    /// `last_sort` as the stored order it came from (a Sort Records order
    /// or dialog) and the base table it sorted, which the Sort Records
    /// dialog shows again on that table; `None` after a sort from names
    /// (Sort Records by Field).
    pub last_sort_spec: Option<LastSortSpec>,
    /// `last_sort` and `last_sort_spec` of the other tables the window
    /// showed: FileMaker Pro keeps the last sort order per table in a
    /// window (26.0.3, the Tasks starter, #840's reading).
    table_sorts: HashMap<u64, TableSort>,
    /// The fields of the record last left (Insert from Last Visited).
    last_visited: HashMap<String, String>,
    /// The record of the active portal row, by its occurrence, as the
    /// step's calculations read it (`Rel::Field` is that row's value;
    /// `None` on the creation row, where it reads empty).
    pub row_record: Option<(u64, Option<u64>)>,
    /// The layout when the top-level script started (Go to Layout
    /// [original layout]).
    pub original_layout: Option<u32>,
}

impl Window {
    /// Start from the UI's window. The record is read again: the user may
    /// have changed it since.
    pub fn adopt(&mut self, state: WindowState) {
        self.first_visit = false;
        self.record = None;
        self.found = None;
        self.sorted = state.sorted;
        self.state = state;
        self.dirty = false;
        self.data_changed = false;
        self.refreshed = false;
        self.objects.clear();
        self.record_stale = false;
        self.scroll.clear();
        self.fronted.clear();
    }

    /// Something visible changed.
    pub fn touch(&mut self) {
        self.dirty = true;
    }

    /// Records or the found set changed: the UI reloads its pages.
    pub fn changed_data(&mut self) {
        self.dirty = true;
        self.data_changed = true;
        self.record = None;
    }

    /// Refresh this window without broadcasting a record write to others.
    /// Retain the old row solely for draft originals until the update is sent;
    /// the next script read fetches its contents again.
    pub fn refresh_data(&mut self) {
        self.dirty = true;
        self.refreshed = true;
        self.record_stale = true;
    }

    pub fn refresh_object(&mut self, refresh: super::ObjectRefresh) {
        self.objects.push(refresh);
        // Subsequent script reads see fresh stored contents as well. Keep the
        // last successful row for draft originals, using Refresh Window's
        // existing retry/protection path without requesting a GUI page reload.
        self.record_stale = true;
        self.touch();
    }

    pub fn can_refresh_object(&self) -> bool {
        self.objects.len() < 4096
    }

    /// Queue without changing record, focus or drafts. Freeze Window can
    /// retain many steps, so bound this queue independently of painting.
    pub fn scroll(&mut self, choice: u16) -> om_data::runtime::HostResult {
        use om_data::runtime::{ErrorCode, HostError};
        if !(1..=5).contains(&choice) {
            return Err(HostError::Error(ErrorCode::INVALID));
        }
        if self.scroll.len() >= 4096 {
            return Err(HostError::Error(ErrorCode::MEMORY));
        }
        self.scroll.push(ScrollRequest {
            choice,
            context: (&self.state).into(),
            field: self.state.focus.clone(),
            field_focus: self.state.field_focus,
        });
        self.touch();
        Ok(())
    }

    /// The update for the UI, if anything changed (always when `force`).
    pub fn take_update(&mut self, force: bool) -> Option<WindowUpdate> {
        if !(self.dirty || force) || (self.frozen && !force) {
            return None;
        }
        let originals = match &self.record {
            Some((fields, record)) => self
                .state
                .drafts
                .keys()
                .filter_map(|name| {
                    let i = fields.iter().position(|f| f.name == *name)?;
                    Some((name.clone(), record.values.get(i).cloned().flatten()))
                })
                .collect(),
            None => FieldChanges::new(),
        };
        let update = WindowUpdate {
            file: 0,
            state: self.state.clone(),
            data_changed: self.data_changed,
            refreshed: self.refreshed,
            objects: std::mem::take(&mut self.objects),
            scroll: std::mem::take(&mut self.scroll),
            fronted: std::mem::take(&mut self.fronted),
            originals,
            found_set: self.found_set.take(),
            background: false,
        };
        self.dirty = false;
        self.data_changed = false;
        self.refreshed = false;
        Some(update)
    }

    /// The current record, read when needed. `None` in an empty found set.
    pub fn current(
        &mut self,
        provider: &mut dyn DataProvider,
    ) -> Result<Option<&(Vec<Field>, Record)>, DataError> {
        self.settle_stray_session(provider);
        if self.record.is_none() || self.record_stale {
            // Keep the last successful row for draft originals if a refresh
            // read fails. Stale rows are never returned as current data.
            let Some(layout) = self.state.layout else {
                return Ok(None);
            };
            let mut page =
                provider.records_with_calculations(layout, self.state.index, ONE, &[])?;
            if page.records.is_empty() && page.total > 0 && self.state.index >= page.total {
                // The found set shrank past the record.
                self.state.index = page.total - 1;
                self.dirty = true;
                page = provider.records_with_calculations(layout, self.state.index, ONE, &[])?;
            }
            if let Some(expected) = self.state.record {
                // The window's record is its id, not its position: another
                // client deleting an earlier record moves it up. A command
                // acts on the record the window showed, never on the one
                // now at its old position, and pending edits stay with it.
                let pending = !self.state.drafts.is_empty() || !self.state.uncommitted.is_empty();
                let read = page.records.first().map(|r| r.id);
                if read != Some(expected) && (read.is_some() || pending) {
                    let gone =
                        || DataError::with_code(101, "The current record is no longer available.");
                    match provider.record_position(layout, expected) {
                        Ok(Some(index)) => {
                            self.state.index = index;
                            self.dirty = true;
                            page = provider.records_with_calculations(layout, index, ONE, &[])?;
                            if page.records.first().map(|r| r.id) != Some(expected) {
                                return Err(gone());
                            }
                        }
                        // Deleted by another client: nothing to act on.
                        Ok(None) => return Err(gone()),
                        // A provider that cannot locate a record reads by
                        // position, unless edits would move with it.
                        Err(e) if !pending && e.code == Some(code::UNSUPPORTED) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            self.record_stale = false;
            self.found = Some(page.total);
            if let Some(record) = page.records.into_iter().next() {
                if self.state.record != Some(record.id) {
                    if self.state.record.is_some() {
                        self.state.drafts.clear();
                        self.stray_session |= self.state.uncommitted.session;
                        self.state.uncommitted = Uncommitted::default();
                        self.state.portal = None;
                    }
                    self.state.record_version = page.versions.get(&record.id).copied();
                    self.state.record = Some(record.id);
                }
                self.record = Some((page.fields, record));
            } else {
                self.record = None;
                self.state.record = None;
            }
        }
        Ok(self.record.as_ref())
    }

    /// The found set's size.
    pub fn found_count(&mut self, provider: &mut dyn DataProvider) -> Result<u64, DataError> {
        if let Some(found) = self.found {
            return Ok(found);
        }
        let Some(layout) = self.state.layout else {
            return Ok(0);
        };
        let page = provider.records(layout, 0, 0)?;
        self.found = Some(page.total);
        Ok(page.total)
    }

    /// The current record's id.
    pub fn record_id(&mut self, provider: &mut dyn DataProvider) -> Result<Option<i64>, DataError> {
        Ok(self.current(provider)?.map(|(_, r)| r.id))
    }

    /// The current record's value of `field` as the user sees it. Fetch
    /// a calculated value only when a command asks for that field;
    /// establishing a script's context must not evaluate every summary.
    pub fn value(
        &mut self,
        provider: &mut dyn DataProvider,
        field: &str,
    ) -> Result<Option<String>, DataError> {
        if let Some(draft) = self.state.drafts.get(field) {
            return Ok(Some(draft.clone().unwrap_or_default()));
        }
        let calculated = provider.calc_schema()?.is_some_and(|schema| {
            let table = self.table.and_then(|id| schema.table(id)).or_else(|| {
                let layout = schema
                    .layouts()
                    .iter()
                    .find(|l| Some(l.id) == self.state.layout)?;
                schema.table_of(layout.occurrence?)
            });
            table
                .and_then(|t| t.fields.values().find(|f| f.name == field))
                .is_some_and(|f| {
                    f.global
                        || f.class == om_data::host::FieldClass::Summary
                        || f.is_unstored_calculation()
                })
        });
        if calculated {
            // Refresh the current index if the found set shrank before
            // selecting this field's calculation.
            if self.current(provider)?.is_none() {
                return Ok(None);
            }
            if let Some(layout) = self.state.layout {
                let page = provider.records_with_calculations(
                    layout,
                    self.state.index,
                    ONE,
                    &[field.to_owned()],
                )?;
                let column = page.fields.iter().position(|f| f.name == field);
                return Ok(column.and_then(|i| {
                    Some(
                        page.records
                            .first()?
                            .values
                            .get(i)?
                            .clone()
                            .unwrap_or_default(),
                    )
                }));
            }
        }
        Ok(self.current(provider)?.and_then(|(fields, record)| {
            let i = fields.iter().position(|f| f.name == field)?;
            Some(record.values.get(i).cloned().flatten().unwrap_or_default())
        }))
    }

    fn prepare_edit(
        &mut self,
        provider: &mut dyn DataProvider,
        cells: BTreeSet<u64>,
        fields: BTreeSet<String>,
    ) -> Result<(), DataError> {
        if self.state.mode != Mode::Browse || self.state.uncommitted.new {
            return Ok(());
        }
        let Some(layout) = self.state.layout else {
            return Ok(());
        };
        let Some(record) = self.state.record else {
            return Ok(());
        };
        let prepared = provider.prepare_record_edit(&om_data::RecordEditRequest {
            baseline: om_data::ContainerEditBaseline {
                layout,
                record,
                version: self.state.record_version,
            },
            cells,
            fields,
        })?;
        if prepared == om_data::EditPreparation::Prepared {
            self.state.uncommitted.session = true;
        }
        Ok(())
    }

    /// Set repetition `cell` of a field of the current record as a script
    /// does (a draft until committed): a first repetition is also a draft
    /// by `name`, which the UI shows.
    pub fn set_cell(
        &mut self,
        provider: &mut dyn DataProvider,
        name: Option<&str>,
        cell: u64,
        text: String,
    ) -> Result<(), DataError> {
        if self.current(provider)?.is_none() {
            return Err(DataError::with_code(101, "There is no current record."));
        }
        if self.state.mode == Mode::Browse && name.is_none() {
            let unchanged = self
                .state
                .uncommitted
                .cells
                .get(&cell)
                .is_some_and(|v| v.as_deref().unwrap_or_default() == text);
            if !unchanged {
                self.prepare_edit(provider, [cell].into_iter().collect(), BTreeSet::new())?;
            }
        }
        if let Some(name) = name {
            self.set(provider, name, text.clone())?;
        }
        self.state
            .uncommitted
            .cells
            .insert(cell, (!text.is_empty()).then_some(text));
        if let Some(table) = self.table {
            self.forget_typed_cell(table, cell);
        }
        self.touch();
        Ok(())
    }

    /// A script set `cell` of `table`: when that is the current record's
    /// table, what the user typed into the cell in a later repetition's
    /// editor (the record's own related draft, #185) gives way, so the
    /// commit writes the script's value, as a first repetition's named
    /// draft does. A cell of another table (a global elsewhere, whose
    /// field ids start again at 1) leaves the typing alone.
    pub(crate) fn forget_typed_cell(&mut self, table: u64, cell: u64) {
        if self.table != Some(table) {
            return;
        }
        let record = self.state.record.and_then(|id| u64::try_from(id).ok());
        let related = &mut self.state.uncommitted.related;
        let own = |d: &RelatedDraft| d.table == table && d.record == record && !d.deleted;
        for draft in related.iter_mut().filter(|d| own(d)) {
            draft.cells.remove(&cell);
            draft.typed.remove(&cell);
            draft.saved.remove(&cell);
        }
        related.retain(|d| !own(d) || !d.cells.is_empty());
    }

    /// The cells a script set on the current record.
    pub fn cells(&self) -> &Cells {
        &self.state.uncommitted.cells
    }

    /// Set a field of the current record (a draft until committed).
    pub fn set(
        &mut self,
        provider: &mut dyn DataProvider,
        field: &str,
        text: String,
    ) -> Result<(), DataError> {
        if self.state.mode == Mode::Find {
            let index = self
                .state
                .find_index
                .min(self.state.find.len().saturating_sub(1));
            if self.state.find.is_empty() {
                self.state.find.push(FindRequest::default());
            }
            self.state.find[index].set(field, text);
            self.touch();
            return Ok(());
        }
        let Some((fields, record)) = self.current(provider)? else {
            return Err(DataError::with_code(101, "There is no current record."));
        };
        let Some(i) = fields.iter().position(|f| f.name == field) else {
            return Err(DataError::with_code(
                102,
                format!("The table has no field named {field:?}."),
            ));
        };
        let stored = record.values.get(i).cloned().flatten().unwrap_or_default();
        if stored == text && !self.state.drafts.contains_key(field) {
            return Ok(());
        }
        self.prepare_edit(
            provider,
            BTreeSet::new(),
            [field.to_owned()].into_iter().collect(),
        )?;
        self.state
            .drafts
            .insert(field.to_owned(), (!text.is_empty()).then_some(text));
        self.touch();
        Ok(())
    }

    /// Whether the current record has unsaved changes (or is new), its
    /// related records included.
    pub fn is_modified(&self) -> bool {
        let open = &self.state.uncommitted;
        !self.state.drafts.is_empty() || !open.cells.is_empty() || !open.related.is_empty()
    }

    /// Whether the current record is one New Record made and no commit
    /// has kept (`Get ( RecordOpenState )` 1).
    pub fn is_new(&self) -> bool {
        self.state.uncommitted.new && self.state.record.is_some()
    }

    /// The fields a new record was made with values in: what it
    /// auto-entered.
    pub fn auto_entered(&self) -> &[String] {
        if self.is_new() {
            &self.state.uncommitted.entered
        } else {
            &[]
        }
    }

    /// Commit the current record's drafts and the related records changed
    /// through it, as a script's writes (`DataProvider::write_record`):
    /// the record first, then its related records in the order they were
    /// changed (a deleted one with `delete_related_record`). `cell_of`
    /// gives a field's cell by name, `None` without a calculation schema
    /// (the provider then writes by name). `privilege_set` is the session's
    /// current set as the step runs (`ScriptWrite::privilege_set`).
    /// `record_numbers` gives a related record to create its
    /// `ScriptWrite::record_number`, by its place in
    /// `Uncommitted::related`. A failure leaves every draft as it was, the
    /// record still open. Nothing to commit is success, but a
    /// new record counts as committed, also when only its related records
    /// are written. Returns the records written or created.
    pub fn commit(
        &mut self,
        provider: &mut dyn DataProvider,
        cell_of: &dyn Fn(&str) -> Option<u64>,
        skip_validation: bool,
        privilege_set: Option<String>,
        record_numbers: &BTreeMap<usize, u64>,
        transaction: Option<&mut Staged>,
    ) -> Result<Vec<Committed>, DataError> {
        // The record session the window's entry opened for its record
        // (`record.prepare_edit`) is the window's before the reply that
        // says so reaches it: the commit writes through it and commits it,
        // rather than leave its writes held there.
        if !self.state.uncommitted.session
            && self.state.record.is_some()
            && provider.record_session_open()
        {
            self.state.uncommitted.session = true;
        }
        // A session set aside for a script's transaction in another window
        // holds a record outside that transaction (script-runtime.md 8.5):
        // committed apart from it, in its own session.
        let entered = self.state.uncommitted.session && provider.enter_record_session();
        let write = CommitWrite {
            cell_of,
            skip_validation,
            privilege_set,
            record_numbers,
            transaction: transaction.filter(|_| !self.state.uncommitted.session),
        };
        let committed = self.commit_record(provider, write);
        if entered {
            provider.leave_record_session();
        }
        committed
    }

    /// [`Self::commit`] with the window's record session open, if it has
    /// one.
    fn commit_record(
        &mut self,
        provider: &mut dyn DataProvider,
        write: CommitWrite<'_>,
    ) -> Result<Vec<Committed>, DataError> {
        self.reserved_made.clear();
        let skip_validation = write.skip_validation;
        if self.state.mode != Mode::Browse {
            // A request is no record: a held new record is settled as
            // leaving it would.
            if std::mem::take(&mut self.state.uncommitted.session) {
                self.stray_session = true;
                self.settle_stray_session(provider);
            }
            self.state.uncommitted.new = false;
            return Ok(Vec::new());
        }
        let Some(layout) = self.state.layout else {
            return Ok(Vec::new());
        };
        let session = self.state.uncommitted.session;
        if session {
            // Its writes go through the session, never another transaction.
            Self::require_session(provider)?;
        }
        if !self.is_modified() {
            if session {
                self.commit_session(provider, skip_validation)?;
            }
            let new = std::mem::take(&mut self.state.uncommitted.new);
            return Ok(self
                .state
                .record
                .filter(|_| new)
                .map(|record| Committed {
                    table: None,
                    record,
                    new: true,
                    deleted: false,
                })
                .into_iter()
                .collect());
        }
        provider.require_records_through(layout)?;
        let Some(id) = self.record_id(provider)? else {
            return Ok(Vec::new());
        };
        // A failed commit leaves the session as it found it: the drafts
        // stay the window's, and the retry writes them once.
        if session {
            provider.mark_record_session();
        }
        let committed = match self.write_through(provider, layout, id, write) {
            Ok(committed) => committed,
            Err(error) => {
                if session {
                    provider.rollback_record_session();
                }
                return Err(error);
            }
        };
        self.state.drafts.clear();
        self.state.typing = None;
        self.state.uncommitted = Uncommitted::default();
        self.changed_data();
        Ok(committed)
    }

    /// [`Self::commit`]'s writes of record `id` through `layout` and of its
    /// related records, then of the record session holding them.
    fn write_through(
        &mut self,
        provider: &mut dyn DataProvider,
        layout: u32,
        id: i64,
        write: CommitWrite<'_>,
    ) -> Result<Vec<Committed>, DataError> {
        let CommitWrite {
            cell_of,
            skip_validation,
            privilege_set,
            record_numbers,
            mut transaction,
        } = write;
        let open = &self.state.uncommitted;
        if open.related.iter().any(|draft| {
            draft
                .creation_parent
                .is_some_and(|(table, record)| open.deletes(table, record))
        }) {
            return Err(missing_related_parent());
        }
        let mut committed = Vec::new();
        // Repetition editors retain numeric cell identities in related drafts.
        // The layout's own repetitions belong to the same record write as its
        // named first-repetition editors, not a second save of that record.
        let own = open.related.iter().find(|draft| {
            Some(draft.table) == self.table
                && draft.record == u64::try_from(id).ok()
                && !draft.deleted
        });
        if !self.state.drafts.is_empty() || !open.cells.is_empty() || own.is_some() {
            let mut cells = open.cells.clone();
            let mut typed = BTreeSet::new();
            for (name, value) in &self.state.drafts {
                if let Some(cell) = cell_of(name) {
                    // A value no script set is the user's typing.
                    if open.cells.get(&cell) != Some(value) {
                        typed.insert(cell);
                    }
                    cells.insert(cell, value.clone());
                }
            }
            if let Some(draft) = own {
                cells.extend(draft.cells.clone());
                typed.extend(draft.typed.iter().copied());
            }
            let pinned = own
                .and_then(|draft| draft.version.map(|version| (draft.table, version)))
                .filter(|(table, version)| provider.retain_related_record(*table, id, *version));
            let result = provider.write_record(&ScriptWrite {
                layout,
                table: None,
                record: Some(id),
                fields: self.state.drafts.clone(),
                cells,
                typed,
                skip_validation,
                privilege_set: privilege_set.clone(),
                // A new record keeps the place it was made at: what its
                // auto-entered Get ( RecordNumber ) was given then (#526).
                record_number: open.new.then_some(open.record_number).flatten(),
                prepared: None,
            });
            if let Some((table, _)) = pinned {
                provider.release_related_record(table, id);
            }
            result?;
            committed.push(Committed {
                table: None,
                record: id,
                new: open.new,
                deleted: false,
            });
        } else if open.new {
            // Created by New Record whatever its commit writes: with only
            // related records to write it is still in the transaction, first
            // (#931).
            committed.push(Committed {
                table: None,
                record: id,
                new: true,
                deleted: false,
            });
        }
        for (index, draft) in open.related.iter().enumerate() {
            if Some(draft.table) == self.table
                && draft.record == u64::try_from(id).ok()
                && !draft.deleted
            {
                continue;
            }
            let record = draft.record.and_then(|r| i64::try_from(r).ok());
            // A new record a refused attempt reserved in a record session
            // or staged in the script transaction keeps its id on retry.
            let reserved = draft.reserved.filter(|_| record.is_none()).filter(|id| {
                transaction
                    .as_ref()
                    .is_some_and(|tx| tx.holds_related(draft.table, *id))
                    || provider.related_record_reserved(draft.table, *id)
            });
            if draft.deleted {
                let Some(record) = record else { continue };
                provider.delete_related_record(draft.table, record)?;
                if let Some(tx) = transaction.as_deref_mut() {
                    tx.deleted(draft.table, record);
                }
                committed.push(Committed {
                    table: Some(draft.table),
                    record,
                    new: false,
                    deleted: true,
                });
                continue;
            }
            // A record the user typed into answers to the version the window
            // showed (or to one the window already kept for it).
            let pinned = record.zip(draft.version).filter(|(record, version)| {
                provider.retain_related_record(draft.table, *record, *version)
            });
            let write = ScriptWrite {
                layout,
                table: Some(draft.table),
                record: record.or(reserved),
                cells: draft.cells.clone(),
                fields: FieldChanges::new(),
                typed: draft.typed.clone(),
                skip_validation,
                privilege_set: privilege_set.clone(),
                record_number: record_numbers.get(&index).copied(),
                // Its preview's values went into the record it holds.
                prepared: draft
                    .prepared
                    .as_ref()
                    .filter(|_| reserved.is_none())
                    .map(|prepared| prepared.id),
            };
            let mut written = provider.write_record(&write);
            if write.prepared.is_some()
                && written
                    .as_ref()
                    .is_err_and(|e| e.code == Some(om_data::found::code::UNSUPPORTED))
            {
                // A preview the provider no longer holds (its file
                // reconnected) refuses the write before anything is sent
                // (#1047): it is written as it would be without one.
                written = provider.write_record(&ScriptWrite {
                    prepared: None,
                    ..write
                });
            }
            if let Some((record, _)) = pinned {
                provider.release_related_record(draft.table, record);
            }
            let written = written?;
            if record.is_none() {
                self.reserved_made.push((index, written.id));
                if let (Some(tx), Some(parent)) =
                    (transaction.as_deref_mut(), draft.creation_parent)
                {
                    tx.created_related(draft.table, written.id, parent);
                }
            }
            committed.push(Committed {
                table: Some(draft.table),
                record: written.id,
                new: record.is_none(),
                deleted: false,
            });
        }
        if self.state.uncommitted.session {
            self.commit_session(provider, skip_validation)?;
        }
        Ok(committed)
    }

    /// Settle a record session the window no longer holds a record in
    /// ([`Self::stray_session`]): committed, as leaving a record commits
    /// it, or reverted when the commit is refused, so that no later write
    /// of the window is held in it.
    fn settle_stray_session(&mut self, provider: &mut dyn DataProvider) {
        if !std::mem::take(&mut self.stray_session) || self.state.uncommitted.session {
            return;
        }
        if !provider.record_session_open() {
            // Set aside while a script's transaction is open: settled once
            // it is back.
            self.stray_session = provider.record_session_held();
            return;
        }
        if provider.commit_record_session(false).is_err() && provider.record_session_open() {
            let _ = provider.revert_transaction();
        }
        self.changed_data();
    }

    /// Commit the window's record session, which holds the new record and
    /// what the commit wrote through it. Refused before anything was sent,
    /// the session stays open and the record held; a session the provider
    /// no longer has (lost with the connection) is an error, the record
    /// gone with it.
    fn commit_session(
        &mut self,
        provider: &mut dyn DataProvider,
        skip_validation: bool,
    ) -> Result<(), DataError> {
        Self::require_session(provider)?;
        provider.commit_record_session(skip_validation)?;
        self.state.uncommitted.session = false;
        Ok(())
    }

    /// The window's record session, open for its commit's writes. One still
    /// set aside for a script's transaction (a provider that cannot enter
    /// it, [`DataProvider::enter_record_session`]) refuses the commit (3),
    /// the record still held: its writes would join that transaction. A
    /// session the provider no longer has (lost with the connection) is an
    /// error, the record gone with it.
    fn require_session(provider: &dyn DataProvider) -> Result<(), DataError> {
        if provider.record_session_open() {
            return Ok(());
        }
        if provider.record_session_held() {
            return Err(DataError::with_code(
                code::UNSUPPORTED,
                "This window's new record is held apart from the open transaction: commit or \
                 revert that transaction first.",
            ));
        }
        Err(DataError::with_code(
            301,
            "The new record is no longer held: it was not saved.",
        ))
    }

    /// What a commit takes from the window, to put back when it fails
    /// after the window let go of it.
    pub fn pending(&self) -> Pending {
        Pending {
            layout: self.state.layout,
            record: self.state.record,
            drafts: self.state.drafts.clone(),
            uncommitted: self.state.uncommitted.clone(),
        }
    }

    /// Put back what [`Self::pending`] took: the record stays open. A new
    /// related record the failed commit wrote keeps the id it was given
    /// (`RelatedDraft::reserved`), for its next write to name when the
    /// session still holds it.
    pub fn restore(&mut self, pending: Pending) {
        self.state.drafts = pending.drafts;
        self.state.uncommitted = pending.uncommitted;
        for (index, id) in std::mem::take(&mut self.reserved_made) {
            if let Some(draft) = self
                .state
                .uncommitted
                .related
                .get_mut(index)
                .filter(|draft| draft.record.is_none() && !draft.deleted)
            {
                draft.reserved = Some(id);
            }
        }
        self.touch();
    }

    /// Whether Revert Transaction removed this window's record: one the
    /// transaction created (`created`, by base table and id: logged by a
    /// commit, or held new by the provider), whatever the window still has
    /// pending on it. A record a window's record session holds apart, or
    /// one an earlier transaction saved, is not among them.
    fn reverted_record(&self, created: &[(u64, i64)]) -> bool {
        let Some(record) = self.state.record else {
            return false;
        };
        self.table
            .is_some_and(|table| created.contains(&(table, record)))
    }

    /// The window lets go of its record and what it has pending on it,
    /// with no call to delete it: the transaction that held it is reverted.
    fn let_go_of_reverted_record(&mut self) {
        self.state.uncommitted = Uncommitted::default();
        self.state.drafts.clear();
        self.state.portal = None;
        self.state.record = None;
        self.changed_data();
        self.found = None;
    }

    /// Revert Transaction removed the current window's record
    /// ([`Self::reverted_record`], #307): it shows the record at its place
    /// in the found set as the revert leaves it, the last one when nothing
    /// is there any more, as Revert Record of a new record does.
    pub fn forget_reverted_record(
        &mut self,
        provider: &mut dyn DataProvider,
        created: &[(u64, i64)],
    ) -> Result<(), DataError> {
        if !self.reverted_record(created) {
            return Ok(());
        }
        self.discard_previews(provider);
        self.let_go_of_reverted_record();
        let found = self.found_count(provider)?;
        self.state.index = self.state.index.min(found.saturating_sub(1));
        Ok(())
    }

    /// [`Self::forget_reverted_record`] for a window that is not current: a
    /// record its own session holds stays, and its found set and place are
    /// read again when it is current.
    pub fn forget_reverted_record_waiting(
        &mut self,
        provider: &mut dyn DataProvider,
        created: &[(u64, i64)],
    ) {
        if self.reverted_record(created) {
            self.discard_previews(provider);
            self.let_go_of_reverted_record();
        }
    }

    /// Drop the script transaction's drafts, keeping the ordinary drafts
    /// that were already pending on this record when the window joined.
    pub fn discard_transaction_drafts(&mut self, provider: &mut dyn DataProvider, before: Pending) {
        if self.state.uncommitted.session {
            return;
        }
        let before = (self.state.layout == before.layout
            && self.state.record == before.record
            && !before.uncommitted.session)
            .then_some(before);
        if before.as_ref().is_some_and(|before| {
            self.state.drafts == before.drafts && self.state.uncommitted == before.uncommitted
        }) {
            return;
        }
        let kept_previews: BTreeSet<_> = before
            .iter()
            .flat_map(|before| &before.uncommitted.related)
            .filter_map(|draft| draft.prepared.as_ref().map(|prepared| prepared.id))
            .collect();
        for draft in &self.state.uncommitted.related {
            if let Some(prepared) = &draft.prepared {
                if !kept_previews.contains(&prepared.id) {
                    provider.discard_prepared(prepared.id);
                }
            }
        }
        self.reserved_made.clear();
        if let Some(before) = before {
            self.restore(before);
        } else {
            self.state.uncommitted = Uncommitted::default();
            self.state.drafts.clear();
        }
        self.state.focus = None;
        self.state.object = None;
        self.state.portal = None;
        self.row_record = None;
        self.state.typing = None;
        self.state.selection = None;
        self.changed_data();
    }

    /// Revert Record: drop the drafts, the related records' too; a new
    /// record is deleted.
    pub fn revert(&mut self, provider: &mut dyn DataProvider) -> Result<(), DataError> {
        // A save that may be partly applied (#1006): Revert Record discards
        // what it did not apply, and the files that saved keep it, a new
        // record among them; nothing is deleted.
        let unsettled = provider.unsettled_save().is_some();
        let new = self
            .state
            .record
            .filter(|_| !unsettled && self.state.uncommitted.new && !self.state.uncommitted.saved);
        if unsettled {
            provider.discard_save()?;
        } else if self.state.uncommitted.session {
            // Nothing of it is on the host: op65 releases its id (#892),
            // also from a session set aside for a script's transaction.
            provider.revert_record_session()?;
        } else if let (Some(id), Some(layout)) = (new, self.state.layout) {
            // Held in a script's transaction (never committed), or a
            // provider without record sessions. A refused delete has not
            // reverted the new record. Keep its drafts and ownership
            // intact so Discard can be retried.
            provider.delete_record_in_script(layout, id)?;
        }
        self.discard_previews(provider);
        self.state.uncommitted = Uncommitted::default();
        self.state.drafts.clear();
        self.state.portal = None;
        self.touch();
        if new.is_some() && self.state.layout.is_some() {
            // Deletion is already confirmed. A later refresh error must
            // not put the deleted record's pending drafts back.
            self.state.record = None;
            self.changed_data();
            self.found = None;
            let found = self.found_count(provider)?;
            self.state.index = self.state.index.min(found.saturating_sub(1));
        }
        Ok(())
    }

    /// Leave the current record for another one (or none): remember its
    /// values for Insert from Last Visited.
    /// The records the window's record was to create are not created (it
    /// was reverted or deleted): their previews go (#1047). Their reserved
    /// serial numbers stay used, a gap.
    fn discard_previews(&self, provider: &mut dyn DataProvider) {
        for draft in &self.state.uncommitted.related {
            if let Some(prepared) = &draft.prepared {
                provider.discard_prepared(prepared.id);
            }
        }
    }

    fn leave_record(&mut self) {
        if let Some((fields, record)) = &self.record {
            self.last_visited = fields
                .iter()
                .zip(&record.values)
                .map(|(f, v)| (f.name.clone(), v.clone().unwrap_or_default()))
                .collect();
        }
        self.record = None;
        self.state.record = None;
        self.state.drafts.clear();
        self.stray_session |= self.state.uncommitted.session;
        self.state.uncommitted = Uncommitted::default();
        self.state.focus = None;
        self.state.object = None;
        self.state.portal = None;
        self.state.typing = None;
        self.state.selection = None;
    }

    /// The last visited record's value of `field`.
    pub fn last_visited(&self, field: &str) -> Option<&str> {
        self.last_visited.get(field).map(String::as_str)
    }

    /// A table the window had not shown starts on the record a local file
    /// was closed on (`DataProvider::take_saved_position`), before the
    /// layout's triggers, a record number or a navigation read the
    /// window's position.
    pub fn start_on_saved_record(&mut self, provider: &mut dyn DataProvider) {
        if !std::mem::take(&mut self.first_visit) {
            return;
        }
        let Some(layout) = self.state.layout else {
            return;
        };
        if let Some(index) = provider.take_saved_position(layout) {
            self.state.index = index;
            if let Some(table) = self.table {
                self.table_index.insert(table, index);
            }
            self.dirty = true;
        }
    }

    /// Make record (or request) `index` current.
    pub fn go_to(&mut self, index: u64) {
        if self.state.mode == Mode::Find {
            self.state.find_index = (index as usize).min(self.state.find.len().saturating_sub(1));
            self.touch();
            return;
        }
        if index != self.state.index || self.record.is_none() {
            self.leave_record();
        }
        self.state.index = index;
        if let Some(table) = self.table {
            self.table_index.insert(table, index);
        }
        self.touch();
    }

    /// The window's layout shows base table `table` now, by a script's
    /// layout change or the UI's (`WorkerHost::sync_layout`). The last sort
    /// order is the table's: the table left keeps its own, and the one
    /// entered gets back what it had.
    pub fn enter_table(&mut self, table: Option<u64>) {
        if table == self.table {
            return;
        }
        let left = (
            std::mem::take(&mut self.last_sort),
            self.last_sort_spec.take(),
        );
        if let Some(previous) = self.table {
            self.table_sorts.insert(previous, left);
        }
        if let Some((sort, spec)) = table.and_then(|t| self.table_sorts.remove(&t)) {
            self.last_sort = sort;
            self.last_sort_spec = spec;
        }
        self.table = table;
    }

    /// Show layout `id` of base table `table`. A layout of the same table
    /// keeps the current record; another table's layout shows the record it
    /// showed last.
    pub fn set_layout(&mut self, id: u32, table: Option<u64>) {
        if Some(id) == self.state.layout {
            return;
        }
        // Layouts whose table is unknown keep the record, as one table.
        let same_table = table == self.table;
        if let Some(previous) = self.table {
            self.table_index.insert(previous, self.state.index);
        }
        self.state.layout = Some(id);
        self.state.preview_page = 0;
        self.enter_table(table);
        if !same_table {
            self.leave_record();
            self.found = None;
            self.sorted = false;
            let shown = table.and_then(|t| self.table_index.get(&t).copied());
            self.first_visit = table.is_some() && shown.is_none();
            self.state.index = shown.unwrap_or(0);
        } else {
            // Same record, other fields.
            self.record = None;
            self.state.focus = None;
            self.state.object = None;
            self.state.portal = None;
        }
        self.touch();
    }

    /// New Record: held in the window's record session until a commit
    /// writes it or Revert releases it (record-writes.md, "A window's
    /// record session"), on a related file's layout by that file. Kept as
    /// new until a commit. `values` and `cells` are what it is given (a
    /// script's, none for New Record); `privilege_set` as in
    /// [`Self::commit`].
    pub fn new_record(
        &mut self,
        provider: &mut dyn DataProvider,
        values: FieldChanges,
        cells: Cells,
        privilege_set: Option<String>,
    ) -> Result<(), DataError> {
        let session = self.open_new_record(provider);
        let record_number = self.next_unsorted_record_number(provider);
        let made = self.make_new_record(
            provider,
            session,
            (values, cells),
            (privilege_set, record_number),
        )?;
        self.adopt_new_record(provider, made)
    }

    /// An unsorted window appends a new record to its current found set.
    /// Its position is unknown when the provider cannot read that set or
    /// keeps it sorted.
    pub(super) fn next_unsorted_record_number(
        &self,
        provider: &mut dyn DataProvider,
    ) -> Option<u64> {
        let layout = self.state.layout?;
        let (_, found) = provider.records_with_summary(layout, 0, 0, &[]).ok()?;
        let found = found.filter(|found| !found.sorted)?;
        found.found.checked_add(1)
    }

    /// Open the record session a new record is held in, as FileMaker Pro
    /// holds it under op71 (write-path.md 1.1): nothing of it is on the
    /// host for another client to change, and Revert releases it (#892). A
    /// session no record of the window holds any more is settled first, as
    /// leaving its record would. Whether one opened: none inside a script's
    /// transaction, which holds the record itself, nor with a provider
    /// without sessions. What the provider takes before the record is
    /// written (Duplicate Record's container references) is the session's
    /// then, released if it is reverted.
    pub fn open_new_record(&mut self, provider: &mut dyn DataProvider) -> bool {
        self.stray_session |= !self.state.uncommitted.session;
        self.settle_stray_session(provider);
        provider.begin_record_commit().is_ok()
    }

    /// Write a new record with `values` and `cells`, held in the session
    /// [`Self::open_new_record`] opened (`session`), else in a script's
    /// transaction; on a layout of a related file's table its file holds it
    /// in the session (#892). A refusal is the error: it is never made at
    /// once instead. A record a provider without transactions makes at once
    /// is saved: Revert keeps it. A failure reverts the session: nothing
    /// was made. `record_number`: the record's place in the found set,
    /// when it is known (`ScriptWrite::record_number`).
    pub fn make_new_record(
        &mut self,
        provider: &mut dyn DataProvider,
        session: bool,
        (values, cells): (FieldChanges, Cells),
        (privilege_set, record_number): (Option<String>, Option<u64>),
    ) -> Result<NewRecord, DataError> {
        let revert = |provider: &mut dyn DataProvider, session: bool| {
            if session {
                let _ = provider.revert_transaction();
            }
        };
        let Some(layout) = self.state.layout else {
            revert(provider, session);
            return Err(DataError::with_code(101, "There is no layout."));
        };
        let write = ScriptWrite {
            layout,
            table: None,
            record: None,
            cells,
            fields: values,
            typed: BTreeSet::new(),
            skip_validation: false,
            privilege_set,
            record_number,
            prepared: None,
        };
        let (record, saved) = match provider.write_record(&write) {
            Ok(record) => (record, !session && !provider.transaction_open()),
            Err(error) => {
                revert(provider, session);
                return Err(error);
            }
        };
        Ok(NewRecord {
            record,
            session,
            saved,
            record_number,
        })
    }

    /// Make the record [`Self::make_new_record`] made the window's current
    /// one, held as new. A refresh failure does not undo it.
    pub fn adopt_new_record(
        &mut self,
        provider: &mut dyn DataProvider,
        made: NewRecord,
    ) -> Result<(), DataError> {
        let created = self.created_record(provider, made.record);
        if self.state.uncommitted.new {
            self.state.uncommitted.session = made.session;
            self.state.uncommitted.saved = made.saved;
            self.state.uncommitted.record_number = made.record_number;
        } else if made.session {
            let _ = provider.revert_transaction();
        }
        created
    }

    /// Adopt a successfully created record before refreshing its found
    /// set. A refresh failure does not undo the create or its ownership.
    pub fn created_record(
        &mut self,
        provider: &mut dyn DataProvider,
        record: Record,
    ) -> Result<(), DataError> {
        let layout = self
            .state
            .layout
            .ok_or_else(|| DataError::with_code(101, "There is no layout."))?;
        let fields = provider
            .records(layout, 0, 0)
            .map(|p| p.fields)
            .unwrap_or_default();
        self.leave_record();
        let entered = fields
            .iter()
            .zip(&record.values)
            .filter(|(_, v)| v.as_deref().is_some_and(|v| !v.is_empty()))
            .map(|(f, _)| f.name.clone())
            .collect();
        self.changed_data();
        self.state.record = Some(record.id);
        self.state.uncommitted = Uncommitted {
            new: true,
            entered,
            ..Uncommitted::default()
        };
        self.found = None;
        let found = self.found_count(provider)?;
        self.state.index = found.saturating_sub(1);
        // A sorted found set may put the record elsewhere.
        if let Ok(page) = provider.records_with_calculations(layout, self.state.index, ONE, &[]) {
            if page.records.first().map(|r| r.id) != Some(record.id) {
                let all = provider.records_with_calculations(layout, 0, found as u32, &[])?;
                if let Some(i) = all.records.iter().position(|r| r.id == record.id) {
                    self.state.index = i as u64;
                }
            }
        }
        Ok(())
    }

    /// Delete Record: the record goes; the next one takes its place.
    pub fn delete(
        &mut self,
        provider: &mut dyn DataProvider,
        transaction: Option<&mut Staged>,
    ) -> Result<(), DataError> {
        // A new record the window's session holds is not on the host:
        // deleting it is releasing it. An existing record's session set
        // aside for a script's transaction goes first: the delete joins
        // that transaction.
        if self.state.uncommitted.session
            && self.state.record.is_some()
            && (self.state.uncommitted.new || !provider.record_session_open())
        {
            if self.state.uncommitted.new {
                return self.revert(provider);
            }
            self.revert(provider)?;
        }
        let (Some(layout), Some(id)) = (self.state.layout, self.record_id(provider)?) else {
            return Err(DataError::with_code(101, "There is no current record."));
        };
        // An existing record's session holds the lock its editing took: the
        // provider deletes the record under it, its edits going with the
        // session. A refusal keeps the edits; a session it ended is no
        // longer the window's.
        let deleted = provider.delete_record_in_script(layout, id);
        let session = self.state.uncommitted.session;
        if session && !provider.record_session_open() && !provider.record_session_held() {
            self.state.uncommitted.session = false;
        }
        deleted?;
        if let (Some(tx), Some(table)) = (transaction, self.table) {
            tx.deleted(table, id);
        }
        // The previews of rows begun in its portals go with the record:
        // read from the drafts, before they are cleared.
        self.discard_previews(provider);
        if session {
            self.state.uncommitted = Uncommitted::default();
            self.state.drafts.clear();
            self.state.portal = None;
            self.touch();
        }
        self.leave_record();
        self.changed_data();
        self.found = None;
        let found = self.found_count(provider)?;
        self.state.index = self.state.index.min(found.saturating_sub(1));
        Ok(())
    }

    /// Record `record` of base table `table` is gone by this client's
    /// doing (deleted in this window or another, or created in a
    /// transaction that was rolled back): a window that showed it shows
    /// the record that takes its place, as the deleting window does.
    /// Pending edits keep the id, so they are never applied to another
    /// record.
    pub fn forget_deleted(&mut self, table: u64, record: i64) {
        if self.table == Some(table)
            && self.state.record == Some(record)
            && self.state.drafts.is_empty()
            && self.state.uncommitted.is_empty()
        {
            self.record = None;
            self.state.record = None;
        }
    }

    /// The ids of the found set's records, in order.
    pub fn found_ids(&mut self, provider: &mut dyn DataProvider) -> Result<Vec<i64>, DataError> {
        let Some(layout) = self.state.layout else {
            return Ok(Vec::new());
        };
        let found = self.found_count(provider)?;
        let page = provider.records_with_calculations(
            layout,
            0,
            u32::try_from(found).unwrap_or(u32::MAX),
            &[],
        )?;
        Ok(page.records.iter().map(|r| r.id).collect())
    }

    /// Change the found set. Its first record becomes current, except after
    /// an omit, where the next record takes the omitted one's place, and
    /// after a Quick Find, where the current record stays current when the
    /// new found set holds it (query-program.md 6.6). A script's find's 401
    /// leaves an empty found set; Quick Find's and a find from the window
    /// leave it as it was. Other refusals preserve the current record and
    /// cached found-set context, just as the provider does.
    pub fn change_found_set(
        &mut self,
        provider: &mut dyn DataProvider,
        change: FoundSetChange,
    ) -> Result<FoundSet, DataError> {
        let Some(layout) = self.state.layout else {
            return Err(DataError::with_code(101, "There is no layout."));
        };
        let keep_index = matches!(change, FoundSetChange::Omit(_));
        let keeps = matches!(
            change,
            FoundSetChange::QuickFind(_) | FoundSetChange::ManualFind { .. }
        );
        let sort = match &change {
            FoundSetChange::Sort(sort) => Some(sort.clone()),
            _ => None,
        };
        // The record the window shows, by identity: positions can have
        // moved since (another client's delete), so read it by position
        // only when the window holds none.
        let quick_find = matches!(change, FoundSetChange::QuickFind(_));
        let current = if quick_find {
            self.state
                .record
                .or_else(|| self.record_id(provider).ok().flatten())
        } else {
            None
        };
        let result = provider.change_found_set(layout, change);
        if result
            .as_ref()
            .is_err_and(|error| keeps || error.code != Some(code::NO_MATCH))
        {
            return result;
        }
        if let Some(sort) = sort {
            self.last_sort = sort;
            self.last_sort_spec = None;
        }
        self.leave_record();
        self.changed_data();
        self.found = None;
        self.found_set = Some(match &result {
            Ok(found) => *found,
            // A find that matched nothing: an empty part of the table.
            Err(_) => FoundSet {
                found: 0,
                total: u64::MAX,
                sorted: false,
            },
        });
        match &result {
            Ok(found) => {
                self.found = Some(found.found);
                self.sorted = found.sorted;
                let kept = current
                    .and_then(|record| provider.record_position(layout, record).ok().flatten())
                    .filter(|position| *position < found.found);
                self.state.index = if keep_index {
                    self.state.index.min(found.found.saturating_sub(1))
                } else {
                    kept.unwrap_or(0)
                };
            }
            Err(_) => self.state.index = 0,
        }
        if let Some(table) = self.table {
            self.table_index.insert(table, self.state.index);
        }
        result
    }

    /// Enter a mode. Find mode starts from one empty request.
    pub fn set_mode(&mut self, mode: Mode) {
        if mode == self.state.mode {
            return;
        }
        if mode == Mode::Find {
            self.state.find = vec![FindRequest::default()];
            self.state.find_index = 0;
        } else if self.state.mode == Mode::Find {
            // `Get ( RequestCount )` keeps the requests' count.
            self.state.requests = self.state.find.len() as u64;
        }
        if mode == Mode::Preview {
            self.state.preview_page = 0;
        }
        self.state.mode = mode;
        self.state.focus = None;
        self.state.object = None;
        self.state.portal = None;
        self.state.typing = None;
        self.state.selection = None;
        self.touch();
    }

    /// The window's id.
    pub fn id(&self) -> u32 {
        self.state.id
    }

    /// New Window: a window `id` named `title` showing what this one shows
    /// (layout, found set, record, mode, view; s35 B1), its own from now
    /// on. Unsaved values stay with this window.
    pub fn opened_from(&self, id: u32, title: String) -> Window {
        let mut window = self.clone();
        window.state.id = id;
        window.state.title = Some(title);
        window.state.drafts.clear();
        window.state.focus = None;
        window.state.object = None;
        window.state.popovers.clear();
        // Its document starts as Browse first shows it, with each tab
        // control's default panel: what its reports print too.
        window.state.panels.clear();
        window.state.portal = None;
        window.state.typing = None;
        window.state.selection = None;
        // Each window has its own `Get ( WindowUUID )`.
        window.state.uuid = om_data::host::window::new_window_uuid();
        window.state.uncommitted = Uncommitted::default();
        window.record = None;
        window.frozen = false;
        window.scroll.clear();
        window.fronted.clear();
        window.dirty = true;
        window.data_changed = true;
        window
    }

    /// The window became current again: its record is read again and the
    /// UI shown it.
    pub fn reenter(&mut self) {
        self.record = None;
        self.found = None;
        self.dirty = true;
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;
    use om_data::runtime::{ErrorCode, HostError};

    /// The last sort order is kept per table in a window, as FileMaker Pro
    /// keeps it (#840's reading: Tasks by Task descending and Assignees by
    /// First Name descending both came back after layout switches).
    #[test]
    fn the_last_sort_order_is_the_tables() {
        let mut window = Window::default();
        let tasks = vec![SortField::new(
            "Task",
            om_data::found::SortOrder::Descending,
        )];
        window.set_layout(1, Some(10));
        window.last_sort = tasks.clone();
        window.set_layout(2, Some(10));
        assert_eq!(window.last_sort, tasks, "another layout of the same table");
        window.set_layout(3, Some(20));
        assert!(window.last_sort.is_empty(), "a table not sorted yet");
        let assignees = vec![SortField::new(
            "First Name",
            om_data::found::SortOrder::Descending,
        )];
        window.last_sort = assignees.clone();
        window.set_layout(1, Some(10));
        assert_eq!(window.last_sort, tasks);
        window.set_layout(3, Some(20));
        assert_eq!(window.last_sort, assignees);
    }

    #[test]
    fn frozen_scroll_retains_order_without_copying_to_new_windows() {
        let mut window = Window::default();
        window.state.layout = Some(7);
        window.state.focus = Some("Code".into());
        window
            .state
            .drafts
            .insert("Code".into(), Some("draft".into()));
        window.frozen = true;
        // A picked tab: the new window's document starts with defaults,
        // which its reports print too.
        window.state.panels.insert((7, 4), 5);
        for choice in [1, 3, 4, 5] {
            window.scroll(choice).unwrap();
        }
        // A panel Go to Object brought to the front waits with the scrolls
        // (#903's review).
        window.fronted.push((7, 4, 5));
        assert!(window.take_update(false).is_none());
        let mut child = window.opened_from(2, "Second".into());
        assert!(child.state.panels.is_empty());
        let first = child.take_update(true).unwrap();
        assert!(first.scroll.is_empty() && first.fronted.is_empty());
        let update = window.take_update(true).unwrap();
        assert_eq!(update.fronted, [(7, 4, 5)]);
        assert_eq!(
            update.scroll.iter().map(|r| r.choice).collect::<Vec<_>>(),
            [1, 3, 4, 5]
        );
        assert!(update
            .scroll
            .iter()
            .all(|r| r.field.as_deref() == Some("Code")));
        assert_eq!(update.state.drafts.get("Code"), Some(&Some("draft".into())));
        assert!(window.take_update(true).unwrap().scroll.is_empty());
    }

    #[test]
    fn invalid_and_excessive_frozen_scrolls_report_errors_and_adoption_drops_stale_effects() {
        let mut window = Window::default();
        assert!(matches!(
            window.scroll(0),
            Err(HostError::Error(ErrorCode::INVALID))
        ));
        assert!(matches!(
            window.scroll(6),
            Err(HostError::Error(ErrorCode::INVALID))
        ));
        for _ in 0..4096 {
            window.scroll(4).unwrap();
        }
        assert!(matches!(
            window.scroll(4),
            Err(HostError::Error(ErrorCode::MEMORY))
        ));
        let state = window.state.clone();
        window.adopt(state);
        window.scroll(1).unwrap();
        assert_eq!(window.take_update(true).unwrap().scroll.len(), 1);
    }
    #[test]
    fn ordinary_form_scrolling_survives_record_navigation_but_selection_and_other_surfaces_do_not()
    {
        let mut window = Window::default();
        window.state.layout = Some(7);
        window.state.index = 0;
        window.scroll(2).unwrap();
        window.scroll(5).unwrap();
        let requests = window.take_update(true).unwrap().scroll;
        let mut next = window.state.clone();
        next.index = 1;
        assert!(requests[0].matches(&next));
        assert!(!requests[1].matches(&next));
        next.view = super::super::View::List;
        assert!(!requests[0].matches(&next));
        next.view = super::super::View::Form;
        next.layout = Some(8);
        assert!(!requests[0].matches(&next));
        next.layout = Some(7);
        next.mode = om_data::runtime::Mode::Find;
        assert!(!requests[0].matches(&next));
        window.state.view = super::super::View::List;
        window.scroll(2).unwrap();
        let list = window.take_update(true).unwrap().scroll;
        next = window.state.clone();
        next.index = 1;
        assert!(!list[0].matches(&next));
    }
}
