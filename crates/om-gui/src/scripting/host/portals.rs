//! Portals in the desktop window (script-steps.md: Go to Portal Row 99, Go
//! to Object 145, Delete Portal Row 104; the Claris pages of those steps;
//! script-runtime.md "Implementation notes (desktop client)", Portals).
//!
//! A portal (`ObjectKind::Portal`, layout-model.md 5.3) shows the records
//! of its table occurrence related to the current record, in the
//! relationship's order (file-metadata.md 4.3); a portal of the layout's
//! own occurrence shows the found set. As FileMaker Server's engine
//! answered (script-runtime.md 13.24, battery s35-portals):
//!
//! - after the related records comes the empty creation row when the
//!   relationship allows creating records and the portal scrolls (portal
//!   option `0x1`); a portal that does not scroll reaches only its visible
//!   rows;
//! - the window keeps the active row ([`PortalRow`]; row 0 when the portal
//!   itself is active): Go to Portal Row, Go to Object and Go to Field set
//!   it, Go to Record keeps its number on the next record, Commit Records
//!   and leaving the layout clear it;
//! - a field step into the portal's occurrence writes the active row's
//!   record, the creation row a new record; Delete Portal Row deletes the
//!   row's record; both belong to the current record and are committed
//!   (or reverted) with it.
//!
//! A portal that sorts its own rows or filters them (`ObjectCalcs`
//! `portal_sort` with portal option `0x8`, `portal_filter`) numbers them as
//! Browse shows them (#192, [`WorkerHost::order_rows`]), and a row a step
//! moves to keeps its record from then on ([`Portal::own_order`]). Only a
//! sort or filter that does not decode leaves the order unknown: the active
//! row then reads no record, and a step that writes, deletes or goes to its
//! record is refused (3; the error alert says why) rather than reach
//! another record.
//!
//! A click in a portal row (a button there, `Start::Button`) brings the
//! row's record from the window, which drew the rows in the portal's own
//! order ([`WorkerHost::enter_clicked_row`]). That row is active, without
//! a field, before the button's parameter and step: found again by its
//! record among the portal's rows (a row of the relationship's order is
//! numbered where the record is now), never by its old number. In a
//! portal of any order the record itself is the active row's while it stays
//! one of the portal's related records (otherwise steps refuse): related
//! fields read it, Set Field writes it, Delete Portal Row deletes it and
//! Go to Related Record starts from it. Moving the active row forgets
//! that identity, as before.

use std::collections::{BTreeMap, BTreeSet};

use om_data::calc::eval::{CalcHost, Context};
use om_data::calc::Value;
use om_data::formula::Expr;
use om_data::host::session::selector;
use om_data::host::{CalcNeed, CalcSchema, FieldClass, Hop};
use om_data::meta::{relation_option, JoinOp};
use om_data::rules::{related_number_self, stores_record_number, RecordNumber, RelatedNumberSelf};
use om_data::runtime::{ErrorCode, FieldRef, HostError, HostResult, RecordMove};
use om_data::specs::SortSpec;
use om_layout::ObjectKind;

use super::super::window::{Cells, PreparedDraft, RelatedDraft, RelatedSelfPreview};
use super::super::PortalRow;
use super::writes::cell_of;
use super::{data_error, note_failure, Act, WorkerHost, BEGUN_RECORD};
use crate::scripting::PortalIdentity as Row;

/// A portal on the window's layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Portal {
    pub object: u64,
    pub occurrence: u64,
    /// "Allow vertical scrolling" (portal option `0x1`); without it only
    /// the visible rows are reached.
    pub scrolls: bool,
    /// "Place row for new record at top" in a portal that does not scroll
    /// (portal option `0x400`, layout-model.md 5.3.2): the rows without a
    /// record come first, as Browse draws them.
    pub new_row_top: bool,
    pub visible: u64,
    /// The records before its initial row (portal option `5`), which
    /// Browse does not draw; its drawn rows are numbered from there.
    pub first: usize,
    /// The portal sorts or filters its own rows by a definition that does
    /// not read, so which record a row shows is not known here (see the
    /// module documentation).
    pub ordered: bool,
    /// It sorts (`0x8`) or filters its rows by a definition that reads: a
    /// row a step moves to is that row's record from then on, as a clicked
    /// row is, so that a step's own change of the record does not move the
    /// row to another record (#192).
    pub own_order: bool,
    /// It filters them.
    pub filtered: bool,
}

/// Where a related field's write goes.
#[derive(Clone, Copy)]
pub(super) enum Target {
    Record(u64),
    Draft(usize),
    Create,
}

struct CreationMatch {
    cells: Cells,
    parent: Option<(u64, u64)>,
}

/// Set Field into a related field that has no related record, through a
/// relationship that does not allow creating one (s35: 101).
const NO_RELATED_RECORD: ErrorCode = ErrorCode::RECORD_MISSING;
/// "Relationship is missing" (no path from the window's occurrence).
const RELATIONSHIP_MISSING: ErrorCode = ErrorCode(103);
/// "One or more required related records are not available": Set Field
/// two relationships away with no record between and no creation through
/// the relationship to it (s49).
const BETWEEN_MISSING: ErrorCode = ErrorCode(415);
/// "Layout object is missing" (Go to Object by a name no object has).
const OBJECT_MISSING: ErrorCode = ErrorCode(116);
/// The portal scrolls (`PortalLayoutObject.flags`, s35 `-word0`/`-word1`).
const PORTAL_SCROLLS: i32 = 0x1;
/// "Place row for new record at top" (FileMaker Pro 26.1).
const PORTAL_NEW_ROW_TOP: i32 = 0x400;
/// The portal sorts its rows by its stored sort order (`portalFlags`
/// `0x8`): a stored order alone does not sort them, as in Browse.
const PORTAL_SORTS: i32 = 0x8;

/// Auto-entry a parent's edit can change a data field by: a creation or
/// modification value (`0x1`), a calculation (`0x8`) or a lookup (`0x400`,
/// schema-path.md 5.5).
const DERIVED_ENTRY: u32 = 0x1 | 0x8 | 0x400;

/// Why a step on the active row of a portal that sorts or filters its
/// rows was refused (the error alert's detail).
const ORDER_UNKNOWN: &str = "The portal sorts or filters its rows, whose order this \
                             window does not know yet: the step would reach another record.";

fn portal(object: &om_layout::LayoutObject) -> Option<Portal> {
    let def = object.portal.as_ref()?;
    let calcs = &object.calcs;
    // The rows are numbered as Browse shows them (`portal_rows`): only a
    // sort or filter that does not read leaves their order unknown here.
    let unread_sort = def.flags & PORTAL_SORTS != 0
        && calcs
            .portal_sort
            .as_deref()
            .is_some_and(|bytes| SortSpec::decode(bytes).is_none());
    let unread_filter = calcs.undecodable.contains(&om_layout::calcs::PORTAL_FILTER);
    let sorts = def.flags & PORTAL_SORTS != 0
        && calcs
            .portal_sort
            .as_deref()
            .and_then(SortSpec::decode)
            .is_some_and(|spec| !spec.fields.is_empty());
    Some(Portal {
        object: object.id,
        occurrence: om_layout::occurrence_id(def.table_alias_key)?,
        scrolls: def.flags & PORTAL_SCROLLS != 0,
        new_row_top: def.flags & PORTAL_NEW_ROW_TOP != 0 && def.flags & PORTAL_SCROLLS == 0,
        visible: (def.num_rows.max(1.0) as u64).max(1),
        first: usize::try_from(def.initial_row.saturating_sub(1)).unwrap_or(0),
        ordered: unread_sort || unread_filter,
        own_order: sorts || calcs.portal_filter.is_some(),
        filtered: calcs.portal_filter.is_some() || unread_filter,
    })
}

/// Why a click in a portal row ran nothing: the row is not the portal's
/// any more.
const ROW_GONE: &str = "The portal row whose button was clicked is no longer in the portal; \
                        the button did not run.";

impl WorkerHost<'_> {
    /// The portals of the window's layout in stacking order, back-most
    /// first (the "first portal in the layout stacking order" Go to Portal
    /// Row uses when none is active).
    pub(super) fn portals(&mut self) -> Vec<Portal> {
        let Some(layout) = self.s.file.window.state.layout else {
            return Vec::new();
        };
        let Some(render) = self.render(layout) else {
            return Vec::new();
        };
        render
            .layout
            .objects
            .objects
            .iter()
            .filter(|o| o.kind() == ObjectKind::Portal)
            .filter_map(portal)
            .collect()
    }

    /// The portal that contains layout object `object` (its parent chain).
    pub(super) fn portal_of(&mut self, object: u64) -> Option<Portal> {
        let layout = self.s.file.window.state.layout?;
        let render = self.render(layout)?;
        let objects = &render.layout.objects;
        let mut at = objects.get(object)?.parent;
        while let Some(id) = at {
            let parent = objects.get(id)?;
            if parent.kind() == ObjectKind::Portal {
                return portal(parent);
            }
            at = parent.parent;
        }
        None
    }

    /// The field objects inside `portal`, top to bottom and left to right:
    /// the first one is the field Go to Portal Row enters.
    fn portal_fields(&mut self, portal: u64) -> Vec<u64> {
        let Some(layout) = self.s.file.window.state.layout else {
            return Vec::new();
        };
        let Some(render) = self.render(layout) else {
            return Vec::new();
        };
        let mut fields: Vec<(f64, f64, u64)> = render
            .layout
            .objects
            .objects
            .iter()
            .filter(|o| o.field_spec().is_some())
            .filter(|o| {
                let mut at = o.parent;
                while let Some(id) = at {
                    if id == portal {
                        return true;
                    }
                    at = render.layout.objects.get(id).and_then(|p| p.parent);
                }
                false
            })
            .filter_map(|o| {
                let bounds = render.layout.absolute_bounds(o.id)?;
                Some((bounds.top, bounds.left, o.id))
            })
            .collect();
        fields.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        fields.into_iter().map(|(_, _, id)| id).collect()
    }

    /// The portal (object) of an active row.
    pub(super) fn portal_by_id(&mut self, object: u64) -> Option<Portal> {
        self.portals().into_iter().find(|p| p.object == object)
    }

    /// The rows of `portal` for the current record, as Browse draws them
    /// (`document::portals::paint_rows`): the related records from the
    /// portal's initial row on, in the relationship's order (without those
    /// a script deleted), the records a script is creating through it, and
    /// the creation row when the relationship allows creating records and
    /// the portal scrolls; a portal that does not scroll has its visible
    /// rows at most, those without a record first when it places the new
    /// record's row at the top. A portal of the layout's own occurrence
    /// shows the found set.
    pub(super) fn portal_rows(&mut self, portal: &Portal) -> Result<Vec<Row>, HostError> {
        Ok(self.numbered_rows(portal)?.1)
    }

    /// [`Self::portal_rows`], numbered after the positions before the
    /// portal's initial row, whether or not records fill them, as Browse
    /// numbers them: the first number less one, and the rows. No step
    /// reaches a number before the initial row [I: whether FileMaker lets
    /// a step reach a record before the initial row, and how it numbers a
    /// row drawn at the top, are unmeasured].
    fn numbered_rows(&mut self, portal: &Portal) -> Result<(u64, Vec<Row>), HostError> {
        let occurrence = portal.occurrence;
        let current = self.s.file.occurrence;
        if Some(occurrence) == current {
            let (window, provider) = self.wp();
            let ids = window.found_ids(provider).map_err(data_error)?;
            let rows = ids
                .into_iter()
                .filter_map(|id| u64::try_from(id).ok())
                .map(Row::Record)
                .collect();
            return Ok((0, rows));
        }
        let offset = u64::try_from(portal.first).unwrap_or(u64::MAX);
        let Some(related) = self.related_rows(occurrence)? else {
            return Ok((offset, Vec::new()));
        };
        let related = self.order_rows(portal, related)?;
        let rows = self.related_display_rows(portal, related.into_iter().skip(portal.first));
        Ok((offset, rows))
    }

    /// The displayed rows after the initial-row offset has been applied
    /// to the saved records.
    fn related_display_rows(
        &mut self,
        portal: &Portal,
        records: impl Iterator<Item = u64>,
    ) -> Vec<Row> {
        let occurrence = portal.occurrence;
        let records = records.map(Row::Record);
        let begun: Vec<Row> = self
            .s
            .file
            .window
            .state
            .uncommitted
            .related
            .iter()
            .filter(|d| d.occurrence == occurrence && d.record.is_none())
            .map(|d| Row::New(d.key))
            .collect();
        if portal.scrolls {
            let mut rows: Vec<_> = records.collect();
            rows.extend(begun);
            if self.creation_allowed(occurrence) {
                rows.push(Row::Creation);
            }
            return rows;
        }
        let mut rows = Vec::new();
        if portal.new_row_top {
            // The rows without a record first, the creation row among
            // them, where Browse draws and numbers them.
            rows.extend(begun);
            if self.creation_allowed(occurrence) {
                rows.push(Row::Creation);
            }
            rows.extend(records);
        } else {
            rows.extend(records);
            rows.extend(begun);
        }
        rows.truncate(usize::try_from(portal.visible).unwrap_or(usize::MAX));
        rows
    }

    /// `records`, the related records `portal` shows in the relationship's
    /// order, in the order and with the filter Browse shows them (#192;
    /// `calc.rs` `portal_records`): its stored sort order when the portal
    /// sorts by it (`0x8`), then its filter, evaluated on each record's
    /// row (numbered from 1 in that order) in the window's record's
    /// context. A sort that cannot be read refuses the step (3) rather than
    /// numbering the rows in another order.
    fn order_rows(&mut self, portal: &Portal, records: Vec<u64>) -> HostResult<Vec<u64>> {
        let Some(layout) = self.s.file.window.state.layout else {
            return Ok(records);
        };
        let Some(object) = self
            .render(layout)
            .and_then(|render| render.layout.objects.get(portal.object).cloned())
        else {
            return Ok(records);
        };
        let sorts = object
            .portal
            .as_ref()
            .is_some_and(|def| def.flags & PORTAL_SORTS != 0);
        let sort = object
            .calcs
            .portal_sort
            .as_deref()
            .filter(|_| sorts)
            .and_then(SortSpec::decode)
            .filter(|spec| !spec.fields.is_empty());
        let mut records = records;
        if let Some(sort) = sort {
            let schema = self.schema().cloned().ok_or(HostError::Unsupported)?;
            let sorted = self.file(|f| {
                om_data::host::sort_records(
                    f.source,
                    &schema,
                    portal.occurrence,
                    records.clone(),
                    &sort,
                    &f.cx(),
                )
            });
            records = match sorted {
                Some(Ok(sorted)) => sorted,
                _ => {
                    note_failure(ORDER_UNKNOWN.into());
                    return Err(ErrorCode::UNAVAILABLE.into());
                }
            };
        }
        if let Some(filter) = &object.calcs.portal_filter {
            let (Some(occurrence), Some(parent)) = (self.s.file.occurrence, {
                let (window, provider) = self.wp();
                window.record_id(provider).map_err(data_error)?
            }) else {
                return Ok(records);
            };
            let parent = u64::try_from(parent).map_err(|_| ErrorCode::RECORD_MISSING)?;
            // The filter reads the script's variables but sets none of them,
            // as Browse evaluates it over copies.
            let variables = self.s.file.variables.clone();
            let mut shown = Vec::with_capacity(records.len());
            for (index, record) in records.iter().enumerate() {
                let context = Context::record(occurrence, parent)
                    .with_portal(portal.occurrence, *record)
                    .with_portal_row_number(index as u64 + 1);
                if om_data::host::layout::holds(self, filter, &context) {
                    shown.push(*record);
                }
            }
            self.s.file.variables = variables;
            records = shown;
        }
        Ok(records)
    }

    /// The records of `occurrence` related to the window's record, in the
    /// relationship's order, without those a script deleted or the
    /// account's set may not view; `None` without a record.
    fn related_rows(&mut self, occurrence: u64) -> Result<Option<Vec<u64>>, HostError> {
        let Some(current) = self.s.file.occurrence else {
            return Ok(None);
        };
        let record = {
            let (window, provider) = self.wp();
            window.record_id(provider).map_err(data_error)?
        };
        let Some(record) = record.and_then(|r| u64::try_from(r).ok()) else {
            return Ok(None);
        };
        let related = self
            .file(|f| f.related_records(current, record, occurrence))
            .unwrap_or(Ok(Vec::new()))
            .map_err(|_| HostError::Error(RELATIONSHIP_MISSING))?;
        let table = self
            .schema()
            .and_then(|s| s.table_of(occurrence))
            .map(|t| t.id);
        // A record the account's set may not view is left out, as the
        // window's portal leaves it out (#695): row numbers follow the rows
        // listed [I]. The provider marks such a record when it reads it, so
        // under a set that limits viewing the table, every candidate is
        // read first; a row is never counted, gone to or deleted on a
        // guess. A read that fails fails the step.
        if let Some(table) = table.filter(|table| self.limits_viewing(*table)) {
            let missing: Vec<CalcNeed> = {
                let cache = self.s.file.cache.borrow();
                related
                    .iter()
                    .filter(|record| !cache.records.contains_key(&(table, **record)))
                    .map(|&record| CalcNeed::Record { table, record })
                    .collect()
            };
            if !missing.is_empty() {
                let data = self
                    .provider
                    .borrow_mut()
                    .calc_data(&missing)
                    .map_err(data_error)?;
                self.s.file.cache.borrow_mut().merge(data);
            }
        }
        let open = &self.s.file.window.state.uncommitted;
        let cache = self.s.file.cache.borrow();
        let withheld = |table: u64, record: u64| {
            cache
                .records
                .get(&(table, record))
                .and_then(|values| values.as_deref())
                .is_some_and(|values| values.no_access)
        };
        Ok(Some(
            related
                .into_iter()
                .filter(|r| {
                    !table.is_some_and(|table| open.deletes(table, *r) || withheld(table, *r))
                })
                .collect(),
        ))
    }

    /// Whether the account's set (a related file's own, for its table)
    /// limits which records of base table `table` it may view.
    fn limits_viewing(&self, table: u64) -> bool {
        let Some(schema) = self.schema() else {
            return false;
        };
        let (access, id) = match schema.table_context(table) {
            None => (schema.authenticated_account.set_access.as_ref(), table),
            Some(file) => (file.account_info.set_access.as_ref(), file.file_id(table)),
        };
        access.is_some_and(|access| !access.views_every_record_id(id))
    }

    /// A click on a button in row `clicked` of a portal (the window's row,
    /// its identity the row's related record): that row becomes the active
    /// row, without a field, before the button's parameter and step, at
    /// the record's row in a portal of the relationship's order, at the
    /// window's row in one that sorts or filters its rows. A row whose
    /// record is not the portal's any more, a portal no longer on the
    /// layout and a portal of the layout's own occurrence are refused
    /// before anything runs (the error alert's text).
    pub(crate) fn enter_clicked_row(&mut self, clicked: PortalRow) -> Result<(), String> {
        if self.s.file.window.state.mode != om_data::runtime::Mode::Browse {
            return Err(ROW_GONE.into());
        }
        if clicked.row == 0 {
            return Err(ROW_GONE.into());
        }
        let Some(portal) = self
            .portal_by_id(clicked.object)
            .filter(|p| p.occurrence == clicked.occurrence)
        else {
            return Err(ROW_GONE.into());
        };
        if Some(portal.occurrence) == self.s.file.occurrence {
            return Err(
                "Buttons in a portal of the layout's own table occurrence are not \
                        supported yet; the button did not run."
                    .into(),
            );
        }
        let refused = |error: HostError| format!("{ROW_GONE} ({error:?})");
        let Some(Row::Record(record)) = clicked.identity else {
            // A row without a record: the creation row, or a record being
            // created through the portal, still one of the portal's rows.
            // It is active by that identity, which the window's row number
            // (in the portal's own order and filter) does not change.
            let row = match clicked.identity {
                Some(row @ (Row::New(_) | Row::Creation))
                    if self.number_of(&portal, row).map_err(refused)?.is_some() =>
                {
                    row
                }
                _ => return Err(ROW_GONE.into()),
            };
            let state = &mut self.s.file.window.state;
            state.focus = None;
            state.object = None;
            state.typing = None;
            state.selection = None;
            state.select = false;
            state.portal = Some(PortalRow {
                object: portal.object,
                occurrence: portal.occurrence,
                row: clicked.row,
                identity: Some(row),
                clicked: true,
                field: clicked.field,
                repetition: 1,
            });
            self.s.file.window.row_record = None;
            self.s.file.window.touch();
            return Ok(());
        };
        let row = if portal.ordered {
            let related = self.related_rows(portal.occurrence).map_err(refused)?;
            if !related.unwrap_or_default().contains(&record) {
                return Err(ROW_GONE.into());
            }
            clicked.row
        } else {
            let Some(number) = self
                .number_of(&portal, Row::Record(record))
                .map_err(refused)?
            else {
                return Err(ROW_GONE.into());
            };
            number
        };
        let state = &mut self.s.file.window.state;
        state.focus = None;
        state.object = None;
        state.typing = None;
        state.selection = None;
        state.select = false;
        state.portal = Some(PortalRow {
            object: portal.object,
            occurrence: portal.occurrence,
            row,
            identity: Some(Row::Record(record)),
            clicked: true,
            field: clicked.field,
            repetition: 1,
        });
        self.s.file.window.row_record = None;
        self.s.file.window.touch();
        Ok(())
    }

    /// A clicked row retains its record in every portal order. If it leaves
    /// the relationship, refuse instead of falling back to the old position.
    fn clicked_record(&mut self, portal: &Portal) -> Result<Option<u64>, HostError> {
        let Some(row) = self
            .s
            .file
            .window
            .state
            .portal
            .filter(|r| r.object == portal.object && r.row > 0 && r.clicked)
        else {
            return Ok(None);
        };
        let record = match row.identity {
            Some(Row::Record(record)) => record,
            // A clicked row without a record ([`Self::clicked_unsaved`]).
            Some(Row::New(_) | Row::Creation) => return Ok(None),
            None => return Err(ErrorCode::RECORD_MISSING.into()),
        };
        if !self
            .related_rows(portal.occurrence)?
            .unwrap_or_default()
            .contains(&record)
        {
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        Ok(Some(record))
    }

    /// The clicked row of `portal` that has no record, the creation row
    /// or a record being created ([`Self::enter_clicked_row`]): steps reach
    /// it by that identity in a portal of any order.
    fn clicked_unsaved(&self, portal: &Portal) -> Option<Row> {
        self.s
            .file
            .window
            .state
            .portal
            .filter(|r| r.object == portal.object && r.row > 0 && r.clicked)
            .and_then(|r| r.identity)
            .filter(|row| matches!(row, Row::New(_) | Row::Creation))
    }

    /// The number (from 1) of the row of `portal` showing `identity`, if it
    /// still shows it where a step reaches it (not before its initial row).
    pub(super) fn number_of(&mut self, portal: &Portal, identity: Row) -> HostResult<Option<u64>> {
        let (offset, rows) = self.numbered_rows(portal)?;
        Ok(rows
            .iter()
            .position(|row| *row == identity)
            .map(|i| offset.saturating_add(i as u64 + 1)))
    }

    /// The row `row` (from 1) of `portal`.
    pub(super) fn row_of(&mut self, portal: &Portal, row: u64) -> Result<Option<Row>, HostError> {
        let (offset, rows) = self.numbered_rows(portal)?;
        Ok(row
            .checked_sub(offset.saturating_add(1))
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| rows.get(i).copied()))
    }

    /// A step that writes, deletes or goes to the record of `portal`'s
    /// active row is refused (3) when the portal sorts or filters its
    /// rows, whose order is not known here.
    fn check_order(&mut self, portal: &Portal) -> HostResult {
        if portal.ordered {
            note_failure(ORDER_UNKNOWN.into());
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        Ok(())
    }

    /// The active row of a portal of `occurrence`, if any, and its portal.
    fn active_row(&mut self, occurrence: u64) -> Option<(Portal, u64)> {
        let active = self
            .s
            .file
            .window
            .state
            .portal
            .filter(|r| r.occurrence == occurrence && r.row > 0)?;
        Some((self.portal_by_id(active.object)?, active.row))
    }

    /// [`Self::check_order`] for the active row of a portal of
    /// `occurrence` (Go to Related Record): a clicked row's record passes,
    /// and another row's when the portal's order is known.
    pub(super) fn check_row_order(&mut self, occurrence: u64) -> HostResult {
        match self
            .s
            .file
            .window
            .state
            .portal
            .filter(|row| row.occurrence == occurrence)
            .and_then(|row| self.portal_by_id(row.object))
        {
            Some(portal) => {
                if self.clicked_record(&portal)?.is_some() {
                    Ok(())
                } else {
                    self.check_order(&portal)
                }
            }
            None => Ok(()),
        }
    }

    /// The saved filter matches for Go to Related Record with "Show
    /// only related records" (#192). The initial row limits navigation
    /// into the portal, but earlier matches still belong to the found set.
    /// `None` when no filtering portal row of `occurrence` is active.
    pub(super) fn filtered_rows(&mut self, occurrence: u64) -> HostResult<Option<Vec<u64>>> {
        let Some((portal, _)) = self.active_row(occurrence) else {
            return Ok(None);
        };
        if !portal.filtered {
            return Ok(None);
        }
        self.check_order(&portal)?;
        let rows = if Some(occurrence) == self.s.file.occurrence {
            self.portal_rows(&portal)?
        } else {
            let Some(related) = self.related_rows(occurrence)? else {
                return Ok(Some(Vec::new()));
            };
            let related = self.order_rows(&portal, related)?;
            if portal.scrolls {
                return Ok(Some(related));
            }
            // Keep the pre-offset matches and the saved rows in the
            // existing visible-row limit for a non-scrolling portal.
            let mut related = related.into_iter();
            let mut rows: Vec<_> = related
                .by_ref()
                .take(portal.first)
                .map(Row::Record)
                .collect();
            rows.extend(self.related_display_rows(&portal, related));
            rows
        };
        Ok(Some(
            rows.into_iter()
                .filter_map(|row| match row {
                    Row::Record(record) => Some(record),
                    Row::New(_) | Row::Creation => None,
                })
                .collect(),
        ))
    }

    /// Go to Portal Row: the active portal, else the first portal in the
    /// stacking order. `Ok(false)` past the first or last row (Exit after
    /// last: 101). A move to a row the portal does not have is 101 and
    /// changes nothing. A portal of the layout's own occurrence moves the
    /// current record, as Go to Record does.
    pub(super) fn go_to_row(&mut self, movement: RecordMove, select: bool) -> HostResult<bool> {
        let active = self.s.file.window.state.portal;
        let portal = match active.and_then(|r| self.portal_by_id(r.object)) {
            Some(portal) => portal,
            None => self
                .portals()
                .first()
                .copied()
                .ok_or(HostError::Error(ErrorCode::UNAVAILABLE))?,
        };
        if Some(portal.occurrence) == self.s.file.occurrence {
            return om_data::runtime::Navigation::go_to_record(self, movement, false);
        }
        // The rows are numbered from the initial row, the first a step
        // reaches; `rows` is the last one's number.
        let (offset, rows) = self.numbered_rows(&portal)?;
        let (rows, first) = (
            offset.saturating_add(rows.len() as u64),
            offset.saturating_add(1),
        );
        let current = active
            .filter(|r| r.object == portal.object)
            .map_or(0, |r| r.row);
        let missing = || Err(HostError::Error(ErrorCode::RECORD_MISSING));
        let row = match movement {
            _ if rows < first => return missing(),
            RecordMove::First => first,
            RecordMove::Last => rows,
            RecordMove::Previous if current <= first => return Ok(false),
            RecordMove::Previous => current - 1,
            RecordMove::Next if current >= rows => return Ok(false),
            RecordMove::Next => (current + 1).max(first),
            RecordMove::Number(n) if n < 1 || (n as u64) < first || n as u64 > rows => {
                return missing()
            }
            RecordMove::Number(n) => n as u64,
        };
        // The step keeps the same field (and repetition) when it changes
        // rows; with none it enters the row's first field; with Select the
        // row itself is active (no field).
        let kept = active.filter(|r| r.object == portal.object && r.field.is_some());
        let (field, repetition) = if select {
            (None, 1)
        } else {
            match kept {
                Some(r) => (r.field, r.repetition),
                None => (self.portal_fields(portal.object).first().copied(), 1),
            }
        };
        // The field the focus is in is left first: its OnObjectExit may
        // keep the focus there, or begin a row that moves the destination,
        // which the move then finds by what it shows.
        let identity = if portal.ordered {
            None
        } else {
            self.row_of(&portal, row)?
        };
        self.run_act(
            Act::Portal {
                row: Some(PortalRow {
                    object: portal.object,
                    occurrence: portal.occurrence,
                    row,
                    identity,
                    clicked: false,
                    field,
                    repetition,
                }),
                select,
            },
            0,
        )?;
        Ok(true)
    }

    /// Go to Record keeps the active row's number on the record it goes
    /// to (s35 A7); a record without that row has no active row and the
    /// move is 101.
    pub(super) fn keep_row(&mut self, kept: Option<PortalRow>) -> HostResult {
        let Some(mut row) = kept else {
            return Ok(());
        };
        let Some(portal) = self.portal_by_id(row.object) else {
            return Ok(());
        };
        if row.row == 0 || self.row_of(&portal, row.row)?.is_some() {
            row.identity = if portal.ordered {
                None
            } else {
                self.row_of(&portal, row.row)?
            };
            row.clicked = portal.own_order && row.identity.is_some();
            self.s.file.window.state.portal = Some(row);
            self.s.file.window.touch();
            return Ok(());
        }
        Err(ErrorCode::RECORD_MISSING.into())
    }

    /// Go to Object: the layout object named `name` (layout-model.md 5.5),
    /// as FileMaker Server's engine answered (s35 A3): a portal becomes the
    /// active object (no row, or the row it had); a field in a portal is
    /// entered in the active row, else the first row (none: 101, the portal
    /// active); a field of the layout's table gets the focus with its
    /// object triggers. No such object is 116, an empty name 5, a
    /// repetition the field does not have 102; a failed step leaves nothing
    /// active.
    pub(super) fn go_to_named_object(&mut self, name: &str, repetition: u32) -> HostResult {
        if name.is_empty() {
            self.leave_objects()?;
            return Err(ErrorCode::INVALID.into());
        }
        let Some(layout) = self.s.file.window.state.layout else {
            return Err(OBJECT_MISSING.into());
        };
        let Some(render) = self.render(layout) else {
            return Err(OBJECT_MISSING.into());
        };
        let Some(object) = render
            .layout
            .objects
            .objects
            .iter()
            .find(|o| o.name.as_deref() == Some(name))
        else {
            self.leave_objects()?;
            return Err(OBJECT_MISSING.into());
        };
        let (id, kind) = (object.id, object.kind());
        let spec = object
            .field_spec()
            .and_then(|s| Some((om_layout::occurrence_id(s.table_key)?, s.field_id()?)));
        if let Some((occurrence, field)) = spec {
            let repetitions = self
                .schema()
                .and_then(|s| s.field(occurrence, field))
                .map_or(1, |f| f.repetitions.max(1));
            if repetition > repetitions {
                self.leave_objects()?;
                return Err(ErrorCode::FIELD_MISSING.into());
            }
        }
        self.front_panels(u64::from(layout), &render.layout.objects, id);
        let field = render.field_bindings.get(&id).cloned();
        if kind == ObjectKind::Portal {
            let row = self
                .s
                .file
                .window
                .state
                .portal
                .filter(|r| r.object == id)
                .map_or(0, |r| r.row);
            let portal = self
                .portal_by_id(id)
                .ok_or(HostError::Error(OBJECT_MISSING))?;
            return self.run_act(
                Act::Portal {
                    row: Some(PortalRow {
                        object: portal.object,
                        occurrence: portal.occurrence,
                        row,
                        identity: None,
                        clicked: false,
                        field: None,
                        repetition: 1,
                    }),
                    select: false,
                },
                0,
            );
        }
        if let Some(portal) = self.portal_of(id) {
            return self.enter_portal_field(portal, id, (repetition.max(1), false));
        }
        let result = match field {
            Some(field) => self.run_act(Act::Focus(Some(field)), 0),
            None => self.leave_objects(),
        };
        if result.is_ok() {
            self.s.file.window.state.object = Some(id);
        }
        result
    }

    /// Bring each tab panel holding object `id` on `layout`, and `id` when
    /// it is one, to the front of its tab control, as Go to Object does in
    /// FileMaker Pro 26: it fronts Inventory's "Units on Hand" panel it
    /// names, and the panel of Meetings' "Topics" portal (#866). A panel
    /// inside another one fronts both [I: not measured]; slide panels are
    /// left as they are (unmeasured). Print and Save Records as PDF print
    /// the panels this leaves in `panels`; the window shows them with its
    /// next update (`fronted`), which Freeze Window holds.
    fn front_panels(&mut self, layout: u64, objects: &om_layout::ObjectStore, id: u64) {
        let mut next = Some(id);
        // A malformed store's parents could loop: no chain is longer than
        // the store.
        for _ in 0..objects.len() {
            let Some(object) = next.and_then(|id| objects.get(id)) else {
                break;
            };
            if let (ObjectKind::TabPanel, Some(control)) = (object.kind(), object.parent) {
                let window = &mut self.s.file.window;
                window.state.panels.insert((layout, control), object.id);
                window.fronted.push((layout, control, object.id));
                window.touch();
            }
            next = object.parent;
        }
    }

    /// No object is active any more (a failed Go to Object or Go to Field).
    fn leave_objects(&mut self) -> HostResult {
        self.run_act(
            Act::Portal {
                row: None,
                select: false,
            },
            0,
        )
    }

    /// Enter repetition `repetition` of field object `object` of `portal`
    /// in its active row, else its first row; a portal without rows becomes
    /// the active object and the step is 101.
    fn enter_portal_field(
        &mut self,
        portal: Portal,
        object: u64,
        (repetition, select): (u32, bool),
    ) -> HostResult {
        // Its first drawn row: the initial row.
        let (offset, rows) = self.numbered_rows(&portal)?;
        let (rows, first) = (
            offset.saturating_add(rows.len() as u64),
            offset.saturating_add(1),
        );
        let row = self
            .s
            .file
            .window
            .state
            .portal
            .filter(|r| r.object == portal.object && r.row > 0)
            .map_or(first, |r| r.row);
        let some = rows >= first;
        self.run_act(
            Act::Portal {
                row: Some(PortalRow {
                    object: portal.object,
                    occurrence: portal.occurrence,
                    row: if some { row.clamp(first, rows) } else { 0 },
                    identity: None,
                    clicked: false,
                    field: some.then_some(object),
                    repetition: if some { repetition } else { 1 },
                }),
                // Go to Field [Select/perform] selects the field's text.
                select: select && some,
            },
            0,
        )?;
        if !some {
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        Ok(())
    }

    /// The name of the active portal object, or of the field in its row
    /// that has the focus (`Get ( ActiveLayoutObjectName )`, s35 A1; empty
    /// when it has none).
    pub(super) fn portal_object_name(
        &mut self,
        state: &super::super::WindowState,
    ) -> Option<String> {
        let row = state.portal?;
        let render = self.render(state.layout?)?;
        let object = render.layout.objects.get(row.field.unwrap_or(row.object))?;
        Some(object.name.clone().unwrap_or_default())
    }

    /// The active row's value of the portal field that has the focus
    /// (`Get ( ActiveFieldContents )`), with the row's unsaved changes.
    pub(super) fn portal_field_text(
        &mut self,
        state: &super::super::WindowState,
    ) -> Option<String> {
        let row = state.portal?;
        let render = self.render(state.layout?)?;
        let spec = render.layout.objects.get(row.field?)?.field_spec()?;
        let occurrence = om_layout::occurrence_id(spec.table_key)?;
        let portal = self.portal_by_id(row.object)?;
        let field = spec.field_id()?;
        // A row the window identified (a button's click, the user's focus)
        // is that row in any order; another one only in the relationship's
        // order (#1027).
        let identified = match self.clicked_record(&portal).ok()? {
            Some(record) => Some(Row::Record(record)),
            None => self.clicked_unsaved(&portal),
        };
        let row_kind = match identified {
            Some(identified) => identified,
            None if portal.ordered => return None,
            None => self.row_of(&portal, row.row).ok()??,
        };
        let record = match row_kind {
            Row::Record(record) => record,
            // A record begun in the creation row: what it holds so far.
            Row::New(key) => {
                let uncommitted = &self.s.file.window.state.uncommitted;
                let draft = &uncommitted.related[uncommitted.draft_index(key)?];
                let cell = cell_of(field, row.repetition.max(1));
                return Some(
                    draft
                        .cells
                        .get(&cell)
                        .cloned()
                        .flatten()
                        .unwrap_or_default(),
                );
            }
            Row::Creation => return Some(String::new()),
        };
        let value = self
            .file(|f| f.field_value(occurrence, record, field, row.repetition.max(1)))?
            .ok()?;
        Some(self.text(&value))
    }

    /// The name and occurrence name of the portal field that has the
    /// focus in `state`, for `Get ( ActiveFieldName )` and `Get (
    /// ActiveFieldTableName )`.
    pub(super) fn portal_field_name(
        &mut self,
        state: &super::super::WindowState,
    ) -> Option<(String, String)> {
        let object = state.portal?.field?;
        let layout = state.layout?;
        let render = self.render(layout)?;
        let spec = render.layout.objects.get(object)?.field_spec()?;
        let occurrence = om_layout::occurrence_id(spec.table_key)?;
        let schema = self.schema()?;
        let field = schema.field(occurrence, spec.field_id()?)?.name.clone();
        let table = schema.occurrence(occurrence)?.name.clone();
        Some((field, table))
    }

    /// Go to Field into a field of a related occurrence (s35 A3): the
    /// field of a portal of that occurrence, entered in the active row,
    /// else the first row (a portal without rows: 101); a related field
    /// outside a portal gets no row. A field not on the layout, or a
    /// repetition it does not have, is 102. A failed step leaves nothing
    /// active.
    pub(super) fn go_to_portal_field(&mut self, field: FieldRef, select: bool) -> HostResult {
        let object = self.s.file.window.state.layout.and_then(|layout| {
            let render = self.render(layout)?;
            render
                .layout
                .objects
                .objects
                .iter()
                .find(|o| {
                    o.field_spec().is_some_and(|s| {
                        om_layout::occurrence_id(s.table_key) == Some(field.occurrence)
                            && s.field_id() == Some(field.field)
                    })
                })
                .map(|o| o.id)
        });
        let repetitions = self
            .schema()
            .and_then(|s| s.field(field.occurrence, field.field))
            .map_or(1, |f| f.repetitions.max(1));
        let Some(object) = object.filter(|_| field.repetition.max(1) <= repetitions) else {
            self.leave_objects()?;
            return Err(ErrorCode::FIELD_MISSING.into());
        };
        self.s.file.window.state.select = select;
        match self.portal_of(object) {
            Some(portal) => {
                let entered =
                    self.enter_portal_field(portal, object, (field.repetition.max(1), select));
                if entered.is_err() {
                    self.s.file.window.state.portal = None;
                }
                entered
            }
            None => self.leave_objects(),
        }
    }

    /// Delete Portal Row (s35 A5): the active row's related record is
    /// deleted with the current record's commit (Revert Record brings it
    /// back; inside a transaction Commit Transaction deletes it), a record
    /// the script was creating is dropped, the creation row deletes
    /// nothing. No active row is error 3. A portal of the layout's own
    /// occurrence deletes the current record, as Delete Record does. No
    /// row is active afterwards.
    pub(super) fn delete_row(&mut self, with_dialog: bool) -> HostResult {
        let Some(active) = self.s.file.window.state.portal.filter(|r| r.row > 0) else {
            return Err(ErrorCode::UNAVAILABLE.into());
        };
        let Some(portal) = self.portal_by_id(active.object) else {
            return Err(ErrorCode::UNAVAILABLE.into());
        };
        let clicked = self.clicked_record(&portal)?;
        let unsaved = self.clicked_unsaved(&portal);
        if clicked.is_none() && unsaved.is_none() {
            self.check_order(&portal)?;
        }
        if with_dialog {
            // FileMaker Pro's alert (client-ui.md, "Delete
            // confirmations"), which has no title of its own.
            self.confirm(
                "",
                "Permanently delete this one related record?".into(),
                "Delete",
            )?;
        }
        if Some(active.occurrence) == self.s.file.occurrence {
            return om_data::runtime::Records::delete_record(self, false);
        }
        let row = match (clicked, unsaved) {
            (Some(record), _) => Some(Row::Record(record)),
            (None, Some(row)) => Some(row),
            (None, None) => self.row_of(&portal, active.row)?,
        };
        match row {
            None | Some(Row::Creation) => {}
            Some(Row::New(key)) => {
                let uncommitted = &mut self.s.file.window.state.uncommitted;
                if let Some(draft) = uncommitted.draft_index(key) {
                    let removed = uncommitted.related.remove(draft);
                    // Its record is not to be created: its preview goes (#1047).
                    if let Some(prepared) = removed.prepared {
                        self.p().discard_prepared(prepared.id);
                    }
                }
            }
            Some(Row::Record(record)) => {
                let table = self
                    .schema()
                    .and_then(|s| s.table_of(active.occurrence))
                    .map(|t| t.id)
                    .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?;
                // The record's own pending changes go with it.
                let related = &mut self.s.file.window.state.uncommitted.related;
                related.retain(|d| !(d.table == table && d.record == Some(record)));
                related.push(RelatedDraft {
                    key: crate::scripting::DraftKey::fresh(),
                    reserved: None,
                    matched_cells: Default::default(),
                    occurrence: active.occurrence,
                    table,
                    record: Some(record),
                    cells: Cells::new(),
                    typed: BTreeSet::new(),
                    version: None,
                    begun: None,
                    matched: false,
                    creation_parent: None,
                    first_related_key: None,
                    related_self: None,
                    deleted: true,
                    saved: BTreeMap::new(),
                    prepared: None,
                });
            }
        }
        self.s.file.window.state.portal = None;
        self.s.file.window.state.focus = None;
        *self.s.file.cache.borrow_mut() = Default::default();
        self.s.file.window.touch();
        Ok(())
    }

    /// The record of the active row of a portal of `occurrence`: what a
    /// related field reads and where `Go to Related Record` starts from
    /// (Claris: the related record of the selected row). `None` without
    /// one, on a row a script is creating, on the creation row, and in a
    /// portal that sorts or filters its rows.
    pub(super) fn active_row_record(&mut self, occurrence: u64) -> Option<u64> {
        let (portal, row) = self.active_row(occurrence)?;
        match self.clicked_record(&portal) {
            Ok(Some(record)) => return Some(record),
            Err(_) => return None,
            Ok(None) => {}
        }
        if portal.ordered || self.clicked_unsaved(&portal).is_some() {
            return None;
        }
        match self.row_of(&portal, row).ok()?? {
            Row::Record(id) => Some(id),
            Row::New(_) | Row::Creation => None,
        }
    }

    /// The related draft (its index in `Uncommitted::related`) a begun
    /// active row of a portal of `occurrence` is creating, as
    /// [`Self::active_row_record`] answers a stored row's record; `None`
    /// for another row, and in a portal that sorts or filters its rows.
    pub(super) fn active_begun_row(&mut self, occurrence: u64) -> Option<usize> {
        let (portal, row) = self.active_row(occurrence)?;
        // A row the window identified is that row in any order (#1027).
        let row = match self.clicked_unsaved(&portal) {
            Some(row) => row,
            None if portal.ordered => return None,
            None => self.row_of(&portal, row).ok()??,
        };
        match row {
            Row::New(key) => self.s.file.window.state.uncommitted.draft_index(key),
            Row::Record(_) | Row::Creation => None,
        }
    }

    /// A script's write of `text` into `cell` of a related record of
    /// `occurrence` (base table `table`): the active portal row's record,
    /// else the first related record, else a new record when the
    /// relationship allows creating one. The write waits for the current
    /// record's commit.
    pub(super) fn set_related(
        &mut self,
        occurrence: u64,
        table: u64,
        cell: u64,
        text: String,
        direct_set_field: bool,
    ) -> HostResult {
        let target = self.related_target(occurrence)?;
        self.set_related_target(target, occurrence, table, cell, text, direct_set_field)
    }

    pub(super) fn set_related_target(
        &mut self,
        target: Target,
        occurrence: u64,
        table: u64,
        cell: u64,
        text: String,
        direct_set_field: bool,
    ) -> HostResult {
        let value = (!text.is_empty()).then_some(text);
        match target {
            Target::Draft(i) => {
                let draft = &self.s.file.window.state.uncommitted.related[i];
                if let Some(key) = draft.first_related_key {
                    if !direct_set_field || key == cell {
                        return Err(ErrorCode::UNAVAILABLE.into());
                    }
                    let mut cells = draft.cells.clone();
                    cells.insert(cell, value);
                    let id = draft.prepared.as_ref().map(|prepared| prepared.id);
                    let (_, _, position) = self
                        .first_related_match_fields(occurrence, table)
                        .filter(|(_, match_key, _)| *match_key == key)
                        .ok_or(ErrorCode::UNAVAILABLE)?;
                    let prepared = self.preview_first_related(table, &cells, id, position)?;
                    let draft = &mut self.s.file.window.state.uncommitted.related[i];
                    draft.cells = cells;
                    draft.prepared = Some(prepared);
                } else {
                    self.s.file.window.state.uncommitted.related[i]
                        .cells
                        .insert(cell, value);
                }
            }
            Target::Record(record) => {
                let related_self = if direct_set_field {
                    self.preview_related_self(occurrence, table, record, cell, &value)?
                } else {
                    None
                };
                // Mixed recovery needs the row accepted before
                // this first script edit, not a read made while saving later.
                let capture = self.s.file.window.state.uncommitted.session
                    && !self
                        .s
                        .file
                        .window
                        .state
                        .uncommitted
                        .related
                        .iter()
                        .any(|d| d.table == table && d.record == Some(record));
                let accepted = if capture {
                    match self
                        .s
                        .file
                        .window
                        .state
                        .related_versions
                        .get(&(table, record))
                        .copied()
                    {
                        Some(version) => Some(version),
                        None => crate::worker::read_related(
                            self.p(),
                            table,
                            i64::try_from(record)
                                .map_err(|_| HostError::Error(ErrorCode::RECORD_MISSING))?,
                        )
                        .map_err(|e| self.failed(e))?
                        .and_then(|r| r.mod_count),
                    }
                } else {
                    None
                };
                let related = &mut self.s.file.window.state.uncommitted.related;
                match related
                    .iter_mut()
                    .find(|d| d.table == table && d.record == Some(record))
                {
                    Some(draft) => {
                        draft.cells.insert(cell, value);
                        if related_self.is_some()
                            || draft
                                .related_self
                                .as_ref()
                                .is_some_and(|known| known.cell == cell)
                        {
                            draft.related_self = related_self;
                        }
                        // The script's value replaces what the user typed.
                        draft.typed.remove(&cell);
                    }
                    None => related.push(RelatedDraft {
                        key: crate::scripting::DraftKey::fresh(),
                        reserved: None,
                        matched_cells: Default::default(),
                        occurrence,
                        table,
                        record: Some(record),
                        cells: Cells::from([(cell, value)]),
                        typed: BTreeSet::new(),
                        version: accepted,
                        begun: None,
                        matched: false,
                        creation_parent: None,
                        first_related_key: None,
                        related_self,
                        deleted: false,
                        saved: BTreeMap::new(),
                        prepared: None,
                    }),
                }
            }
            Target::Create => {
                let CreationMatch { mut cells, parent } = self.match_cells(occurrence)?;
                let first_related = if direct_set_field {
                    self.first_related_key(occurrence, table, &cells, cell)
                } else {
                    None
                };
                cells.insert(cell, value);
                let prepared = first_related
                    .map(|(_, position)| self.preview_first_related(table, &cells, None, position))
                    .transpose()?;
                // An active creation row is now the new record's (a new
                // creation row follows it).
                self.s
                    .file
                    .window
                    .state
                    .uncommitted
                    .related
                    .push(RelatedDraft {
                        key: crate::scripting::DraftKey::fresh(),
                        reserved: None,
                        matched_cells: Default::default(),
                        occurrence,
                        table,
                        record: None,
                        cells,
                        typed: BTreeSet::new(),
                        version: None,
                        begun: None,
                        matched: true,
                        creation_parent: parent,
                        first_related_key: first_related.map(|(key, _)| key),
                        related_self: None,
                        deleted: false,
                        saved: BTreeMap::new(),
                        prepared,
                    });
                // A clicked creation row, active by its identity, is the new
                // record's row now: later steps reach that record.
                let state = &mut self.s.file.window.state;
                let created = state.uncommitted.related.last().map(|d| d.key);
                if let Some(active) = state.portal.as_mut().filter(|row| {
                    row.occurrence == occurrence
                        && row.clicked
                        && row.identity == Some(Row::Creation)
                }) {
                    active.identity = created.map(Row::New);
                }
            }
        }
        *self.s.file.cache.borrow_mut() = Default::default();
        self.s.file.window.touch();
        Ok(())
    }

    /// Evaluate the measured replacing Self assignment to the stored match
    /// after an intermediate Commit. Preview through the same record rules
    /// that will write it, leaving the proposed cell untouched for Commit.
    fn preview_related_self(
        &mut self,
        occurrence: u64,
        table: u64,
        record: u64,
        cell: u64,
        input: &Option<String>,
    ) -> HostResult<Option<RelatedSelfPreview>> {
        let Some(definitions) = self.schema().and_then(|schema| schema.table(table)) else {
            return Ok(None);
        };
        if definitions.fields.get(&cell).and_then(related_number_self)
            != Some(RelatedNumberSelf::Replace)
            || definitions.fields.values().any(stores_record_number)
            || !self.has_stored_related_parent()
            || self.first_related_match_fields(occurrence, table).is_none()
            || self.p().local_path().is_none()
        {
            return Ok(None);
        }
        let layout = self
            .s
            .file
            .window
            .state
            .layout
            .ok_or(ErrorCode::UNAVAILABLE)?;
        let write = om_data::ScriptWrite {
            layout,
            table: Some(table),
            record: Some(i64::try_from(record).map_err(|_| ErrorCode::RECORD_MISSING)?),
            cells: Cells::from([(cell, input.clone())]),
            privilege_set: self.current_set_name(),
            single_field_step: true,
            ..Default::default()
        };
        let pending = self
            .p()
            .compute_pending(&write, &[])
            .map_err(|error| self.failed(error))?
            .filter(|pending| pending.proposed_self.contains(&cell))
            .ok_or(ErrorCode::UNAVAILABLE)?;
        let value = pending
            .values
            .get(&cell)
            .cloned()
            .ok_or(ErrorCode::UNAVAILABLE)?;
        Ok(Some(RelatedSelfPreview {
            cell,
            input: input.clone(),
            value,
        }))
    }

    /// The match fields of the measured local, direct unsorted equality
    /// relationship. The child must belong to a different table in this
    /// file. The copied global value wins even over the measured replacing
    /// UUID default: the provider keeps explicit cells on a scripted create.
    fn first_related_match_fields(
        &self,
        occurrence: u64,
        table: u64,
    ) -> Option<(u64, u64, RecordNumber)> {
        let schema = self.schema()?;
        let current = self.s.file.occurrence?;
        let own = schema.table_of(current)?;
        if table == own.id
            || schema.table_file(table).unwrap_or(0) != 0
            || schema.table_file(own.id).unwrap_or(0) != 0
            || self.limits_viewing(table)
        {
            return None;
        }
        let path = schema.path(current, occurrence)?;
        let [hop] = path.as_slice() else {
            return None;
        };
        let [predicate] = hop.predicates.as_slice() else {
            return None;
        };
        if hop.sorted() || predicate.op != JoinOp::Equal || !creates_through(schema, hop) {
            return None;
        }
        let from = schema.field(current, predicate.left)?;
        let to = schema.field(occurrence, predicate.right)?;
        if from.class != FieldClass::Data
            || from.value_type != om_data::calc::ValueType::Text
            || from.options != 0x0001_0000
            || from.repetitions != 1
            || to.class != FieldClass::Data
            || to.value_type != om_data::calc::ValueType::Text
            || to.global
            || to.repetitions != 1
        {
            return None;
        }
        let replacing_uuid = to.options == 0xa000_0088
            && from.validation_options == 0
            && to.validation_options == 0
            && to
                .auto_enter
                .as_ref()
                .is_some_and(|formula| matches!(formula.polish, Expr::Get(selector::UUID)));
        let plain_key = to.auto_enter.is_none() && to.options & 0x0000_07ff == 0;
        if !plain_key && !replacing_uuid {
            return None;
        }
        let fields = &schema.table(table)?.fields;
        let has_position = fields.values().any(|field| {
            stores_record_number(field)
                && (!replacing_uuid
                    || field.options == 0x8008_0000 && field.validation_options == 0)
        });
        // The UUID key also occurs in the separately measured indexed
        // Number default case, with no stored Position on the table.
        let has_self_default = replacing_uuid
            && !fields.values().any(stores_record_number)
            && fields
                .values()
                .any(|field| related_number_self(field).is_some());
        let position = if replacing_uuid {
            RecordNumber::LocalFirstRelatedUuid
        } else {
            RecordNumber::LocalFirstRelated
        };
        (has_position || has_self_default).then_some((from.id, to.id, position))
    }

    fn has_stored_related_parent(&self) -> bool {
        let state = &self.s.file.window.state;
        state.record.is_some()
            && state.portal.is_none()
            && !state.uncommitted.new
            && state.drafts.is_empty()
            && state.uncommitted.cells.is_empty()
            && self.s.file.transaction.is_none()
    }

    /// Prepare the early read only for the measured first related creation.
    fn first_related_key(
        &mut self,
        occurrence: u64,
        table: u64,
        cells: &Cells,
        target: u64,
    ) -> Option<(u64, RecordNumber)> {
        let (_, key, position) = self.first_related_match_fields(occurrence, table)?;
        if target == key
            || !self.has_stored_related_parent()
            || !self.s.file.window.state.uncommitted.related.is_empty()
        {
            return None;
        }
        let value = cells.get(&key)?.as_ref()?;
        if value.is_empty() || value.contains(['\r', '\n']) {
            return None;
        }
        self.p().local_path().is_some().then_some((key, position))
    }

    fn preview_first_related(
        &mut self,
        table: u64,
        cells: &Cells,
        prepared: Option<om_data::PreparedId>,
        position: RecordNumber,
    ) -> HostResult<PreparedDraft> {
        let layout = self
            .s
            .file
            .window
            .state
            .layout
            .ok_or(ErrorCode::UNAVAILABLE)?;
        let write = om_data::ScriptWrite {
            layout,
            table: Some(table),
            cells: cells.clone(),
            privilege_set: self.current_set_name(),
            record_number: Some(position),
            prepared,
            ..Default::default()
        };
        let preview = self
            .p()
            .preview_new_record(&write)
            .map_err(|error| self.failed(error))?
            .ok_or(ErrorCode::UNAVAILABLE)?;
        Ok(PreparedDraft {
            id: preview.id,
            values: preview.values,
            over: cells.clone(),
            context: None,
        })
    }

    /// Pending first match for the window's current record. The current
    /// global key still has to match the one copied into the draft.
    pub(super) fn first_related_draft(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Option<usize> {
        let state = &self.s.file.window.state;
        if Some(occurrence) != self.s.file.occurrence || state.record != i64::try_from(record).ok()
        {
            return None;
        }
        let (index, draft) = state
            .uncommitted
            .related
            .iter()
            .enumerate()
            .find(|(_, draft)| {
                draft.occurrence == related
                    && draft.first_related_key.is_some()
                    && draft.record.is_none()
                    && !draft.deleted
            })?;
        let (source, key, _) = self.first_related_match_fields(related, draft.table)?;
        if draft.first_related_key != Some(key) {
            return None;
        }
        let value = draft.cells.get(&key)?.as_deref()?;
        let global = self
            .file(|f| f.field_value(occurrence, record, source, 1))?
            .ok()?;
        matches!(global, Value::Text(text) if text.as_str() == value).then_some(index)
    }

    pub(super) fn related_creation_numbers(&mut self) -> BTreeMap<usize, RecordNumber> {
        let mut numbers = self.begun_record_numbers();
        let state = &self.s.file.window.state;
        let [draft] = state.uncommitted.related.as_slice() else {
            return numbers;
        };
        if draft.first_related_key.is_none()
            || !self.has_stored_related_parent()
            || !draft
                .prepared
                .as_ref()
                .is_some_and(|prepared| prepared.over == draft.cells)
        {
            return numbers;
        }
        if let Some((occurrence, record)) = self
            .s
            .file
            .occurrence
            .zip(state.record.and_then(|record| u64::try_from(record).ok()))
        {
            if self.first_related_draft(occurrence, record, draft.occurrence) == Some(0)
                && self
                    .file(|f| f.related_records(occurrence, record, draft.occurrence))
                    .is_some_and(|rows| rows.is_ok_and(|rows| rows.is_empty()))
            {
                if let Some((_, _, position)) =
                    self.first_related_match_fields(draft.occurrence, draft.table)
                {
                    numbers.insert(0, position);
                }
            }
        }
        numbers
    }

    /// The draft cell of the target resolved before an Insert upload.
    pub(super) fn related_draft_cell_at(
        &self,
        target: Target,
        table: u64,
        cell: u64,
    ) -> Option<String> {
        let related = &self.s.file.window.state.uncommitted.related;
        let draft = match target {
            Target::Draft(i) => related.get(i)?,
            Target::Record(record) => related
                .iter()
                .find(|d| d.table == table && d.record == Some(record))?,
            Target::Create => return None,
        };
        draft.cells.get(&cell).cloned().flatten()
    }

    /// The container references the related records the window's record
    /// keeps uncommitted hold, by base table: what a revert releases.
    pub(super) fn related_container_drafts(&mut self) -> Vec<(u64, String)> {
        let Some(schema) = self.schema().cloned() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for draft in &self.s.file.window.state.uncommitted.related {
            let Some(table) = schema.table(draft.table) else {
                continue;
            };
            for (cell, value) in &draft.cells {
                let container = table
                    .fields
                    .values()
                    .any(|f| f.id == *cell && f.value_type == om_data::calc::ValueType::Container);
                if let Some(value) = value.as_ref().filter(|_| container && !draft.deleted) {
                    if om_data::containers::reference(value).is_some() {
                        out.push((draft.table, value.clone()));
                    }
                }
            }
        }
        out
    }

    /// The draft `key` names, as a write's target: missing (101) once it
    /// is gone.
    fn draft_target(&self, key: crate::scripting::DraftKey) -> Result<Target, HostError> {
        self.s
            .file
            .window
            .state
            .uncommitted
            .draft_index(key)
            .map(Target::Draft)
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))
    }

    pub(super) fn related_target(&mut self, occurrence: u64) -> Result<Target, HostError> {
        let current = self
            .s
            .file
            .occurrence
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        if let Some(active) = self.s.file.window.state.portal {
            if active.occurrence == occurrence {
                if let Some(portal) = self.portal_by_id(active.object) {
                    if let Some(record) = self.clicked_record(&portal)? {
                        return Ok(Target::Record(record));
                    }
                    match self.clicked_unsaved(&portal) {
                        Some(Row::New(key)) => return self.draft_target(key),
                        Some(Row::Creation) => return Ok(Target::Create),
                        _ => {}
                    }
                    self.check_order(&portal)?;
                    match self.row_of(&portal, active.row)? {
                        Some(Row::Record(id)) => return Ok(Target::Record(id)),
                        Some(Row::New(key)) => return self.draft_target(key),
                        Some(Row::Creation) => return Ok(Target::Create),
                        None => {}
                    }
                }
            }
        }
        let record = {
            let (window, provider) = self.wp();
            window.record_id(provider).map_err(data_error)?
        }
        .and_then(|r| u64::try_from(r).ok())
        .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        let first = self
            .related_records(current, record, occurrence)
            .map_err(|_| HostError::Error(RELATIONSHIP_MISSING))?
            .first()
            .copied();
        if let Some(first) = first {
            if let Some(index) = first
                .checked_sub(BEGUN_RECORD)
                .and_then(|index| usize::try_from(index).ok())
            {
                return Ok(Target::Draft(index));
            }
            return Ok(Target::Record(first));
        }
        let pending = self
            .s
            .file
            .window
            .state
            .uncommitted
            .related
            .iter()
            .position(|d| {
                d.occurrence == occurrence && d.record.is_none() && d.first_related_key.is_none()
            });
        Ok(pending.map_or(Target::Create, Target::Draft))
    }

    /// The records the user began in portals' creation rows
    /// (`RelatedDraft::begun`) take their match fields from the
    /// current record as it is now, as a script's do ([`Self::match_cells`]);
    /// a value the user typed into a match field stays. A record a step
    /// created keeps the match fields it was created with
    /// (`RelatedDraft::matched`). A relationship that does not allow
    /// creating records fails (101).
    pub(super) fn match_begun_records(&mut self) -> HostResult {
        let begun: Vec<(usize, u64)> = self
            .s
            .file
            .window
            .state
            .uncommitted
            .related
            .iter()
            .enumerate()
            .filter(|(_, d)| d.record.is_none() && d.begun.is_some() && !d.matched)
            .map(|(i, d)| (i, d.occurrence))
            .collect();
        for (index, occurrence) in begun {
            let CreationMatch { cells, parent } = self.match_cells(occurrence)?;
            let draft = &mut self.s.file.window.state.uncommitted.related[index];
            draft.creation_parent = parent;
            for (cell, value) in cells {
                let typed = draft.typed.contains(&cell)
                    && draft.cells.get(&cell).is_some_and(Option::is_some);
                if !typed {
                    draft.cells.insert(cell, value);
                    draft.matched_cells.insert(cell);
                }
            }
        }
        Ok(())
    }

    /// `Get ( RecordNumber )` of the records the commit creates from
    /// portals' creation rows, by their place in
    /// `Uncommitted::related` (`ScriptWrite::record_number`, #1038): only
    /// in the shape FileMaker Pro 26.0.3 was measured in, where a row
    /// typed into the creation row of an ordinary portal stores its place
    /// after the records related before the save (1 with none, 3 with
    /// two; auto-entry.md, "Get ( RecordNumber )"). That shape is:
    /// - one record begun in a creation row, the only record of its table
    ///   the commit writes (the window's own record too, through a
    ///   relationship to its own table);
    /// - a direct relationship from the window's stored record that does
    ///   not sort, from match fields the commit does not change, nor can
    ///   change through a calculation or auto-entry;
    /// - every portal of the occurrence on the layout neither sorts (no
    ///   `0x8`) nor filters its rows;
    /// - an account's set that views every record of the table.
    ///
    /// Any other record has none, which refuses only a write whose formula
    /// evaluates the selector.
    fn begun_record_numbers(&mut self) -> BTreeMap<usize, RecordNumber> {
        let mut numbers = BTreeMap::new();
        let open = &self.s.file.window.state.uncommitted;
        if open.new {
            return numbers;
        }
        let begun: Vec<(usize, u64, u64)> = open
            .related
            .iter()
            .enumerate()
            .filter(|(_, d)| d.record.is_none() && d.begun.is_some() && !d.matched && !d.deleted)
            // A stored record of the table changed in the same commit can
            // change which records are related (calculations see the
            // window's drafts).
            .filter(|(index, d)| {
                !open
                    .related
                    .iter()
                    .enumerate()
                    .any(|(other, o)| other != *index && o.table == d.table)
            })
            .map(|(index, d)| (index, d.occurrence, d.table))
            .collect();
        for (index, occurrence, table) in begun {
            if let Some(number) = self.begun_record_number(occurrence, table) {
                numbers.insert(index, RecordNumber::Window(number));
            }
        }
        numbers
    }

    /// [`Self::begun_record_numbers`] of the one record begun through
    /// `occurrence` (of base table `table`).
    fn begun_record_number(&mut self, occurrence: u64, table: u64) -> Option<u64> {
        if self.limits_viewing(table) || !self.plain_portals(occurrence) {
            return None;
        }
        let schema = self.schema()?.clone();
        let current = self.s.file.occurrence?;
        let path = schema.path(current, occurrence)?;
        let [hop] = path.as_slice() else {
            return None;
        };
        if hop.sorted() || !self.creation_allowed(occurrence) {
            return None;
        }
        // A changed match field relates the record to others than the
        // stored ones [I: unmeasured]: one the commit writes, or one whose
        // value the commit's changes can give it anew (a calculation, a
        // global, an auto-entered modification value, calculation or
        // lookup), whatever the commit changes.
        let own = schema.table_of(current)?;
        let state = &self.s.file.window.state;
        let parent_written = !state.drafts.is_empty() || !state.uncommitted.cells.is_empty();
        // Through a relationship to the window's own table, the window's
        // record is a record of the table the commit writes too: its edit
        // can change which records are related (a self-join).
        if own.id == table && parent_written {
            return None;
        }
        let pending = parent_written || state.uncommitted.related.len() > 1;
        let changes = |field: u64| {
            let Some(def) = own.fields.get(&field) else {
                return true;
            };
            let derived = def.class != FieldClass::Data
                || def.formula.is_some()
                || def.auto_enter.is_some()
                || def.summary.is_some()
                || def.global
                || def.options & DERIVED_ENTRY != 0;
            state.drafts.contains_key(&def.name)
                || state
                    .uncommitted
                    .cells
                    .keys()
                    .any(|cell| *cell == field || cell / 256 == field)
                || (derived && pending)
        };
        if hop.predicates.iter().any(|p| changes(p.left)) {
            return None;
        }
        let record = {
            let (window, provider) = self.wp();
            window.record_id(provider).ok()??
        };
        let record = u64::try_from(record).ok()?;
        let related = self
            .file(|f| f.related_records(current, record, occurrence))?
            .ok()?;
        u64::try_from(related.len()).ok()?.checked_add(1)
    }

    /// Whether the layout shows `occurrence` in a portal, and every
    /// portal of it there neither sorts (`0x8`) nor filters its rows.
    fn plain_portals(&mut self, occurrence: u64) -> bool {
        let Some(layout) = self.s.file.window.state.layout else {
            return false;
        };
        let Some(render) = self.render(layout) else {
            return false;
        };
        let mut shown = false;
        for object in &render.layout.objects.objects {
            let Some(def) = object.portal.as_ref() else {
                continue;
            };
            if om_layout::occurrence_id(def.table_alias_key) != Some(occurrence) {
                continue;
            }
            let calcs = &object.calcs;
            if def.flags & PORTAL_SORTS != 0
                || calcs.portal_filter.is_some()
                || calcs.undecodable.contains(&om_layout::calcs::PORTAL_FILTER)
            {
                return false;
            }
            shown = true;
        }
        shown
    }

    /// The records the user began in portals' creation rows as an ordinary
    /// commit's OnObjectSave script reads them (#1047): with their match
    /// fields from the current record ([`Self::match_begun_records`]), and
    /// with what their own file's rules enter, computed by its provider
    /// without writing anything (`DataProvider::preview_new_record`;
    /// FileMaker Pro 26.0.3 showed a new portal row's auto-entered value to
    /// that script, auto-entry.md "Get ( RecordNumber )"). A row previewed
    /// before is computed again only when its cells changed since, under the
    /// same preview; one the provider no longer holds (its file
    /// reconnected) is previewed anew. A row the provider cannot preview
    /// reads as before: what was typed, and its match fields.
    pub(crate) fn prepare_begun_records(&mut self) {
        if self.match_begun_records().is_err() {
            // The commit reports it.
            return;
        }
        let Some(layout) = self.s.file.window.state.layout else {
            return;
        };
        let begun: Vec<usize> = self
            .s
            .file
            .window
            .state
            .uncommitted
            .related
            .iter()
            .enumerate()
            .filter(|(_, d)| d.record.is_none() && d.begun.is_some() && !d.deleted)
            .map(|(index, _)| index)
            .collect();
        // A global field the user typed reaches the session only with the
        // commit (`take_typed_globals`): a preview would read the value
        // before it, which the commit does not. Nothing is previewed then,
        // and what an earlier preview showed is not shown.
        if self.typed_globals_pending() {
            for &index in &begun {
                if let Some(prepared) =
                    &mut self.s.file.window.state.uncommitted.related[index].prepared
                {
                    prepared.values.clear();
                    prepared.over.clear();
                    prepared.context = None;
                }
            }
            return;
        }
        let numbers = self.begun_record_numbers();
        let set = self.current_set_name();
        // A row's preview reads the others' (a self-join, an aggregate):
        // each pass previews again, under its own preview, the rows whose
        // cells or other records changed since, until none did. A cycle of
        // rows reading each other is cut after a pass per row.
        for _ in 0..=begun.len() {
            let mut changed = false;
            for &index in &begun {
                changed |= self.prepare_begun_record(index, layout, &numbers, &set);
            }
            if !changed {
                break;
            }
        }
        // What a row still reading changed records showed is not shown.
        for &index in &begun {
            let context = self.pending_records(index);
            let draft = &mut self.s.file.window.state.uncommitted.related[index];
            if let Some(prepared) = &mut draft.prepared {
                if prepared.over != draft.cells || prepared.context != context {
                    prepared.values.clear();
                }
            }
        }
    }

    /// [`Self::prepare_begun_records`] of the begun row at `index` in
    /// `Uncommitted::related`: whether it was previewed (again).
    fn prepare_begun_record(
        &mut self,
        index: usize,
        layout: u32,
        numbers: &BTreeMap<usize, RecordNumber>,
        set: &Option<String>,
    ) -> bool {
        let context = self.pending_records(index);
        let draft = &self.s.file.window.state.uncommitted.related[index];
        if draft
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.over == draft.cells && prepared.context == context)
        {
            return false;
        }
        let mut write = om_data::ScriptWrite {
            layout,
            table: Some(draft.table),
            record: None,
            cells: draft.cells.clone(),
            fields: om_data::FieldChanges::new(),
            typed: draft.typed.clone(),
            skip_validation: false,
            privilege_set: set.clone(),
            record_number: numbers.get(&index).copied(),
            prepared: draft.prepared.as_ref().map(|prepared| prepared.id),
            single_field_step: false,
        };
        let preview_over = |host: &mut Self, write: &om_data::ScriptWrite| match &context {
            Some(pending) => host.p().preview_new_record_over(write, pending),
            None => host.p().preview_new_record(write),
        };
        let mut preview = preview_over(self, &write);
        if write.prepared.is_some()
            && preview
                .as_ref()
                .is_err_and(|e| e.code == Some(om_data::found::code::UNSUPPORTED))
        {
            write.prepared = None;
            preview = preview_over(self, &write);
        }
        let draft = &mut self.s.file.window.state.uncommitted.related[index];
        match preview {
            Ok(Some(preview)) => {
                draft.prepared = Some(PreparedDraft {
                    id: preview.id,
                    values: preview.values,
                    over: draft.cells.clone(),
                    context,
                });
                true
            }
            // It cannot be previewed now: the preview it has, if any, still
            // names its reservations, with nothing known to show.
            Ok(None) => {
                if let Some(prepared) = &mut draft.prepared {
                    prepared.values.clear();
                    prepared.over = draft.cells.clone();
                    prepared.context = context;
                }
                false
            }
            // What it showed was computed over what changed since: it is not
            // shown (the rows reading it are previewed again), and the next
            // prepare tries again; the commit reports a failure that lasts.
            Err(_) => match &mut draft.prepared {
                Some(prepared) => {
                    let shown = !prepared.values.is_empty();
                    prepared.values.clear();
                    prepared.over.clear();
                    prepared.context = None;
                    shown
                }
                None => false,
            },
        }
    }

    /// The user typed a global field into the window's record or a related
    /// record, which the session has not taken yet (`take_typed_globals`).
    fn typed_globals_pending(&self) -> bool {
        let Some(schema) = self.schema() else {
            return false;
        };
        let state = &self.s.file.window.state;
        let own = self.s.file.occurrence.and_then(|o| schema.table_of(o));
        let own_global = own.is_some_and(|table| {
            state
                .drafts
                .keys()
                .any(|name| table.fields.values().any(|f| f.global && f.name == *name))
        });
        let related_global = state.uncommitted.related.iter().any(|draft| {
            schema.table(draft.table).is_some_and(|table| {
                draft.cells.keys().any(|cell| {
                    table
                        .fields
                        .get(cell)
                        .or_else(|| table.fields.get(&(cell / 256)))
                        .is_some_and(|f| f.global)
                })
            })
        });
        own_global || related_global
    }

    /// The window's records it has not saved, besides the begun row at
    /// `except` in `Uncommitted::related`, as the preview of that row reads
    /// them (`DataProvider::preview_new_record_over`, #1063): the window's
    /// own record with what was typed or set into it, the portal rows
    /// edited through it, and the other rows begun in its portals (with what
    /// their own previews entered), each by its table's id in this file.
    /// `None` when the window holds a change they cannot say: a record to
    /// delete with the commit, or a record New Record made and has not
    /// saved yet. The preview then reads no other record, as without them.
    fn pending_records(&mut self, except: usize) -> Option<Vec<om_data::PendingRecord>> {
        let ids = self.layout_field_ids();
        let table = self.s.file.window.table;
        let record = {
            let (window, provider) = self.wp();
            window.record_id(provider).ok().flatten()
        };
        let state = &self.s.file.window.state;
        let open = &state.uncommitted;
        if open.new || open.related.iter().any(|draft| draft.deleted) {
            return None;
        }
        let mut pending = Vec::new();
        // The window's own record, its cells as its commit writes them
        // (`Window::commit`): a value no script set is the user's typing.
        let mut cells = open.cells.clone();
        let mut typed = BTreeSet::new();
        for (name, value) in &state.drafts {
            if let Some(cell) = ids.get(name).copied() {
                if open.cells.get(&cell) != Some(value) {
                    typed.insert(cell);
                }
                cells.insert(cell, value.clone());
            }
        }
        for draft in &open.related {
            if Some(draft.table) == table
                && draft.record == record.and_then(|id| u64::try_from(id).ok())
            {
                cells.extend(draft.cells.clone());
                typed.extend(draft.typed.iter().copied());
            }
        }
        if !cells.is_empty() {
            pending.push(om_data::PendingRecord {
                table: table?,
                record: Some(record?),
                cells,
                typed,
            });
        }
        for (index, draft) in open.related.iter().enumerate() {
            if index == except
                || Some(draft.table) == table
                    && draft.record == record.and_then(|id| u64::try_from(id).ok())
            {
                continue;
            }
            pending.push(om_data::PendingRecord {
                table: draft.table,
                record: match draft.record {
                    Some(record) => Some(i64::try_from(record).ok()?),
                    None => None,
                },
                cells: draft.previewed_cells(),
                typed: draft.typed.clone(),
            });
        }
        Some(pending)
    }

    /// The match fields of a record created through the relationships from
    /// the window's occurrence to `occurrence`: each `=` predicate's
    /// related field takes the value of the record it is made from (Claris
    /// "Creating and changing relationships": the related record is created
    /// "based on the match field in the current table"). The relationship to
    /// `occurrence` must allow creating its records ("Allow creation of
    /// records in this table via this relationship" on that side), else
    /// 101, whatever the others allow (s49). Through more than one
    /// relationship the record is made from the first related record of the
    /// occurrence before it, along the path. Also returns the stored parent
    /// of a multi-hop draft so its commit can detect a later deletion.
    /// Missing middle records that FileMaker would create (s49), and middle
    /// records still in a draft, are unsupported here (#41).
    fn match_cells(&mut self, occurrence: u64) -> Result<CreationMatch, HostError> {
        let schema = self.schema().cloned().ok_or(HostError::Unsupported)?;
        let current = self
            .s
            .file
            .occurrence
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        let path = schema
            .path(current, occurrence)
            .ok_or(HostError::Error(RELATIONSHIP_MISSING))?;
        let Some((hop, before)) = path.split_last() else {
            return Err(NO_RELATED_RECORD.into());
        };
        if !creates_through(&schema, hop) {
            return Err(NO_RELATED_RECORD.into());
        }
        let record = {
            let (window, provider) = self.wp();
            window.record_id(provider).map_err(data_error)?
        }
        .and_then(|r| u64::try_from(r).ok())
        .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        // The record the new one is made from: the window's, else the first
        // related record of each occurrence on the way.
        let mut parent = record;
        let mut creation_parent = None;
        for between in before {
            let records = self
                .file(|f| f.related_records(between.from, parent, between.to))
                .ok_or(HostError::Unsupported)?
                .map_err(|_| HostError::Error(RELATIONSHIP_MISSING))?;
            let table = schema.table_of(between.to).ok_or(RELATIONSHIP_MISSING)?.id;
            let open = &self.s.file.window.state.uncommitted;
            let Some(next) = records.iter().copied().find(|r| !open.deletes(table, *r)) else {
                if !records.is_empty() {
                    return Err(BETWEEN_MISSING.into());
                }
                let pending = open.related.iter().any(|draft| {
                    draft.occurrence == between.to && draft.record.is_none() && !draft.deleted
                });
                return Err(if pending || creates_through(&schema, between) {
                    HostError::Unsupported
                } else {
                    BETWEEN_MISSING.into()
                });
            };
            parent = next;
            creation_parent = Some((table, parent));
        }
        let mut cells = Cells::new();
        for predicate in hop.predicates.iter().filter(|p| p.op == JoinOp::Equal) {
            let from = schema
                .field(hop.from, predicate.left)
                .ok_or(BETWEEN_MISSING)?;
            let text = if from.is_unstored_calculation() {
                let to = schema
                    .field(hop.to, predicate.right)
                    .ok_or(BETWEEN_MISSING)?;
                // A computed key must supply one scalar text value that
                // can be copied unchanged and read back as a match key.
                if from.formula.is_none()
                    || from.repetitions != 1
                    || to.class != FieldClass::Data
                    || to.global
                    || to.value_type != om_data::calc::ValueType::Text
                {
                    return Err(HostError::Unsupported);
                }
                let values = self
                    .file(|f| {
                        f.source.source_match_values(
                            f.schema,
                            hop.from,
                            parent,
                            predicate.left,
                            &f.cx_for(hop.from),
                        )
                    })
                    .ok_or(HostError::Unsupported)?
                    .map_err(|_| BETWEEN_MISSING)?;
                let [Value::Text(key)] = values.as_slice() else {
                    return Err(BETWEEN_MISSING.into());
                };
                if key.as_str().contains(['\r', '\n']) {
                    return Err(HostError::Unsupported);
                }
                key.as_str().to_owned()
            } else {
                let value = self
                    .file(|f| f.field_value(hop.from, parent, predicate.left, 1))
                    .ok_or(HostError::Unsupported)?
                    .map_err(|_| BETWEEN_MISSING)?;
                self.text(&value)
            };
            cells.insert(
                cell_of(predicate.right, 1),
                (!text.is_empty()).then_some(text),
            );
        }
        Ok(CreationMatch {
            cells,
            parent: creation_parent,
        })
    }

    /// Whether the relationship from the window's occurrence to
    /// `occurrence` allows creating its records: a direct relationship
    /// whose "Allow creation of records in this table via this
    /// relationship" is on for that side (file-metadata.md 4.1).
    pub(super) fn creation_allowed(&mut self, occurrence: u64) -> bool {
        let (Some(schema), Some(current)) = (self.schema(), self.s.file.occurrence) else {
            return false;
        };
        let Some(path) = schema.path(current, occurrence) else {
            return false;
        };
        let [hop] = path.as_slice() else {
            return false;
        };
        creates_through(schema, hop)
    }
}

/// Whether `hop`'s relationship allows creating records of its far
/// occurrence (file-metadata.md 4.1).
fn creates_through(schema: &CalcSchema, hop: &Hop) -> bool {
    schema.relationships().iter().any(|r| {
        (r.left == hop.from
            && r.right == hop.to
            && r.options & relation_option::CREATE_LEFT_TO_RIGHT != 0)
            || (r.right == hop.from
                && r.left == hop.to
                && r.options & relation_option::CREATE_RIGHT_TO_LEFT != 0)
    })
}

#[cfg(test)]
mod tests {
    use om_layout::{LayoutObject, ObjectCalcs, PortalObject};

    use super::*;

    fn portal_of(calcs: ObjectCalcs, flags: i32) -> Portal {
        let object = LayoutObject {
            id: 1,
            portal: Some(PortalObject {
                table_alias_key: 1,
                flags,
                ..PortalObject::default()
            }),
            calcs,
            ..LayoutObject::default()
        };
        portal(&object).expect("a portal")
    }

    /// The worker numbers a portal's rows in its own sort and filter order
    /// (#192): only a definition that does not read leaves it unknown.
    #[test]
    fn only_a_portal_order_that_does_not_read_is_unknown() {
        let sort = |fields: Vec<om_data::specs::SortEntry>| ObjectCalcs {
            portal_sort: Some(
                SortSpec {
                    keep_sorted: true,
                    blanks_last: false,
                    fields,
                }
                .encode(),
            ),
            ..ObjectCalcs::default()
        };
        let field = om_data::specs::SortEntry {
            field: om_data::specs::SpecField {
                occurrence: Some(0xd0_0002),
                field: 1,
                repetition: 1,
            },
            summary: None,
            value_list: None,
            descending: false,
            locale: 0,
        };
        let filter = ObjectCalcs {
            portal_filter: Some(om_formula::Formula {
                context: None,
                formatted: None,
                polish: om_formula::Expr::Empty,
            }),
            ..ObjectCalcs::default()
        };
        for flags in [0, PORTAL_SORTS] {
            assert!(!portal_of(ObjectCalcs::default(), flags).ordered);
            assert!(!portal_of(sort(Vec::new()), flags).ordered);
            assert!(!portal_of(sort(vec![field.clone()]), flags).ordered);
            let filtered = portal_of(filter.clone(), flags);
            assert!(!filtered.ordered && filtered.filtered);
        }
        // A stored order that does not read sorts only with option 0x8.
        let unread = ObjectCalcs {
            portal_sort: Some(vec![0, 0, 0]),
            ..ObjectCalcs::default()
        };
        assert!(!portal_of(unread.clone(), 0).ordered);
        assert!(portal_of(unread, PORTAL_SORTS).ordered);
        let unread_filter = portal_of(
            ObjectCalcs {
                undecodable: vec![om_layout::calcs::PORTAL_FILTER],
                ..ObjectCalcs::default()
            },
            0,
        );
        assert!(unread_filter.ordered && unread_filter.filtered);
    }
}
