//! Portals in a server script's window (script-runtime.md 13.24, battery
//! s35-portals; the desktop's `scripting/host/portals.rs` follows the
//! same runs).
//!
//! A portal (layout-model.md 5.3) shows the records of its table
//! occurrence related to the window's record, in the relationship's
//! order. After them comes the empty creation row when the relationship
//! allows creating records and the portal scrolls (option `0x1`); a
//! portal that does not scroll reaches only its visible rows. The window
//! keeps the active row ([`ActiveRow`], row 0 when the portal itself is
//! active): Go to Portal Row, Go to Object and Go to Field set it, Go to
//! Record keeps its number (and the field entered) on the next record,
//! and another layout, Commit Records, a deleted record, a mode change
//! or a field outside the portal clears it. A step's calculation reads
//! the active row's record for a direct reference to the portal's
//! related fields ([`ServerHost::active_row_record`]); `GetNthRecord`,
//! aggregates and field calculations read the related set as before.
//!
//! Which portal an object is in is read from its parents alone, once per
//! step ([`ObjectIndex`]), so a layout's size bounds a step's work and its
//! geometry cannot move a field out of its portal.
//!
//! Not yet: Set Field into a related field and Delete Portal Row (still
//! 3), and portals that sort or filter their own rows, whose rows are
//! refused (3) rather than numbered in an order that may not be
//! FileMaker's.

use std::collections::{HashMap, HashSet};

use om_calc::CalcError;
use om_data::meta::relation_option;
use om_layout::{Layout, LayoutObject, ObjectKind};
use om_scripts::runtime::{ErrorCode, FieldRef, HostError, HostResult, Mode, RecordMove};

use super::get::Focus;
use super::ServerHost;

/// "Layout object is missing" (Go to Object by a name no object has).
const OBJECT_MISSING: ErrorCode = ErrorCode(116);
/// The portal scrolls (`PortalLayoutObject.flags`, s35 `-word0`/`-word1`).
const PORTAL_SCROLLS: i32 = 0x1;
/// The portal sorts its rows by its stored sort order (`kSortRows`).
const PORTAL_SORTS: i32 = 0x8;
/// The portal filters its rows by its stored filter (`kFilterRows`): a
/// filter kept with the option off does not apply (layout-model.md 5.3.2).
const PORTAL_FILTERS: i32 = 0x80;

/// The window's active portal row: the portal object and its occurrence,
/// and the row (from 1; 0 when the portal itself is active, no row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ActiveRow {
    pub object: u64,
    pub occurrence: u64,
    pub row: u64,
    /// The portal object's name, `Get ( ActiveLayoutObjectName )` while
    /// no field of it is entered.
    pub name: String,
}

/// A portal on the window's layout.
#[derive(Clone, Debug)]
struct Portal {
    object: u64,
    occurrence: u64,
    scrolls: bool,
    visible: u64,
    /// It sorts or filters its own rows.
    own_order: bool,
    name: String,
}

fn portal(object: &LayoutObject) -> Option<Portal> {
    let def = object.portal.as_ref()?;
    let calcs = &object.calcs;
    let sorts = def.flags & PORTAL_SORTS != 0;
    let filters = def.flags & PORTAL_FILTERS != 0
        && (calcs.portal_filter.is_some()
            || calcs.undecodable.contains(&om_layout::calcs::PORTAL_FILTER));
    Some(Portal {
        object: object.id,
        occurrence: om_layout::occurrence_id(def.table_alias_key)?,
        scrolls: def.flags & PORTAL_SCROLLS != 0,
        visible: (def.num_rows.max(1.0) as u64).max(1),
        own_order: sorts || filters,
        name: object.name.clone().unwrap_or_default(),
    })
}

/// The occurrence and field an object shows.
fn shown_field(object: &LayoutObject) -> Option<(u64, u64)> {
    let spec = object.field_spec()?;
    Some((
        om_layout::occurrence_id(spec.table_key)?,
        om_hbam::serial::raw_of_id(spec.field_key)?,
    ))
}

/// A layout's objects by id, with the portal each lies in, read once: by
/// following its parents (a chain that loops lies in none), not by where
/// it is drawn.
struct ObjectIndex<'l> {
    layout: &'l Layout,
    by_id: HashMap<u64, &'l LayoutObject>,
    portal_of: HashMap<u64, u64>,
}

impl<'l> ObjectIndex<'l> {
    fn new(layout: &'l Layout) -> Self {
        let by_id: HashMap<u64, &LayoutObject> =
            layout.objects.objects.iter().map(|o| (o.id, o)).collect();
        // The nearest portal above each object, `None` for none; each
        // chain is walked once, the objects on it answered together.
        let mut known: HashMap<u64, Option<u64>> = HashMap::new();
        for object in &layout.objects.objects {
            let mut chain = Vec::new();
            let mut at = object.parent;
            let mut seen = HashSet::new();
            let found = loop {
                let Some(id) = at else { break None };
                let Some(parent) = by_id.get(&id) else {
                    break None;
                };
                // A parent that is a portal answers before what is known
                // above it.
                if parent.kind() == ObjectKind::Portal {
                    break Some(id);
                }
                if let Some(known) = known.get(&id) {
                    break *known;
                }
                if !seen.insert(id) {
                    break None;
                }
                chain.push(id);
                at = parent.parent;
            };
            for id in chain {
                known.insert(id, found);
            }
            known.entry(object.id).or_insert(found);
        }
        let portal_of = known
            .into_iter()
            .filter_map(|(id, portal)| Some((id, portal?)))
            .collect();
        ObjectIndex {
            layout,
            by_id,
            portal_of,
        }
    }

    /// The portal object `id` lies in.
    fn portal_of(&self, id: u64) -> Option<u64> {
        self.portal_of.get(&id).copied()
    }

    /// The objects of portal `portal` that show a field, in the order
    /// FileMaker enters them: top to bottom, then left to right (those
    /// without bounds last, in the store's order).
    fn portal_fields(&self, portal: u64) -> Vec<&'l LayoutObject> {
        let mut fields: Vec<(f64, f64, usize, &LayoutObject)> = self
            .layout
            .objects
            .objects
            .iter()
            .enumerate()
            .filter(|(_, o)| o.field_spec().is_some() && self.portal_of(o.id) == Some(portal))
            .map(|(at, o)| match self.layout.absolute_bounds(o.id) {
                Some(bounds) => (bounds.top, bounds.left, at, o),
                None => (f64::INFINITY, f64::INFINITY, at, o),
            })
            .collect();
        fields.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then(a.1.total_cmp(&b.1))
                .then(a.2.cmp(&b.2))
        });
        fields.into_iter().map(|(.., o)| o).collect()
    }

    fn get(&self, id: u64) -> Option<&'l LayoutObject> {
        self.by_id.get(&id).copied()
    }
}

impl ServerHost {
    /// The window's layout as decoded from the file.
    fn decoded_layout(&self) -> Option<std::sync::Arc<Layout>> {
        let schema = self.design()?;
        let window = self.window.as_ref()?;
        let id = self.schema.layouts.get(window.layout)?.id;
        schema
            .layouts()
            .iter()
            .find(|l| u64::from(l.id) == id)?
            .layout
            .clone()
    }

    /// The portals of `layout` in stacking order, back-most first (the
    /// "first portal in the layout stacking order" Go to Portal Row uses
    /// when none is active).
    fn portals_of(layout: &Layout) -> Vec<Portal> {
        om_data::host::layout_objects::objects_in_order(layout)
            .into_iter()
            .filter(|o| o.kind() == ObjectKind::Portal)
            .filter_map(portal)
            .collect()
    }

    /// Whether the relationship from the window's occurrence to
    /// `occurrence` lets a portal create records there: one hop whose
    /// relationship allows creation on that side.
    fn creation_allowed(&self, occurrence: u64) -> bool {
        let (Some(schema), Some(window)) = (self.calc_schema(), self.window.as_ref()) else {
            return false;
        };
        let Some(path) = schema.path(window.occurrence, occurrence) else {
            return false;
        };
        let [hop] = path.as_slice() else {
            return false;
        };
        schema.relationships().iter().any(|r| {
            (r.left == hop.from
                && r.right == hop.to
                && r.options & relation_option::CREATE_LEFT_TO_RIGHT != 0)
                || (r.right == hop.from
                    && r.left == hop.to
                    && r.options & relation_option::CREATE_RIGHT_TO_LEFT != 0)
        })
    }

    /// The related records `portal` shows for the window's record, in the
    /// relationship's order, and how many rows it has: those records,
    /// then the creation row when allowed and the portal scrolls; a
    /// portal that does not scroll has its visible rows at most. A portal
    /// that orders its own rows is refused (3): its rows are not numbered
    /// here yet.
    fn portal_rows(&self, portal: &Portal) -> HostResult<(Vec<u64>, u64)> {
        self.require_view()?;
        if portal.own_order {
            return Err(HostError::Unsupported);
        }
        let window = self.window.as_ref().ok_or(ErrorCode::WINDOW_MISSING)?;
        let Some(record) = window.record() else {
            return Ok((Vec::new(), 0));
        };
        let records = self
            .related_set(window.occurrence, record, portal.occurrence)
            .map_err(|_| HostError::Error(ErrorCode(103)))?;
        let mut rows = records.len() as u64;
        if portal.scrolls && self.creation_allowed(portal.occurrence) {
            rows += 1;
        }
        if !portal.scrolls {
            rows = rows.min(portal.visible);
        }
        Ok((records, rows))
    }

    /// The record a step's calculation reads for a direct reference to a
    /// related field of `related` from record `record` of `occurrence`,
    /// when the window's active portal row is one of those records' rows:
    /// `Some(Some(id))` its record, `Some(None)` the creation row (an
    /// empty record), `None` no active row there (the first related record
    /// is read, as before).
    pub(super) fn active_row_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Option<Option<u64>>, CalcError> {
        let Some(window) = self.window.as_ref() else {
            return Ok(None);
        };
        let Some(row) = window.portal.as_ref().filter(|r| r.row > 0) else {
            return Ok(None);
        };
        if window.mode != Mode::Browse
            || row.occurrence != related
            || window.occurrence != occurrence
            || window.record() != Some(record)
        {
            return Ok(None);
        }
        let records = self.related_set(occurrence, record, related)?;
        let index = usize::try_from(row.row - 1).ok();
        Ok(Some(index.and_then(|index| records.get(index).copied())))
    }

    /// Make `row` of `portal` active, with `field` (an object of the
    /// portal) entered in repetition `repetition`, or none.
    fn enter_row(
        &mut self,
        portal: &Portal,
        row: u64,
        field: Option<&LayoutObject>,
        repetition: u32,
    ) {
        self.focus = field.and_then(|object| focus_on(object, repetition));
        if let Some(window) = self.window.as_mut() {
            window.portal = Some(ActiveRow {
                object: portal.object,
                occurrence: portal.occurrence,
                row,
                name: portal.name.clone(),
            });
        }
    }

    /// Go to Portal Row (99): in the active portal, else the first portal
    /// in stacking order (3 without one); First, Last (the creation row
    /// when there is one), Previous and Next (`Ok(false)` past either
    /// end, the row kept), or a row by number (101 outside the rows). The
    /// step keeps the field (and repetition) entered in the portal, else
    /// enters the row's first field; with Select the row is active without
    /// a field. A portal of the layout's own occurrence moves the found
    /// set.
    pub(super) fn go_to_portal_row(
        &mut self,
        movement: RecordMove,
        select: bool,
    ) -> HostResult<bool> {
        let layout = self.decoded_layout().ok_or(HostError::Unsupported)?;
        let index = ObjectIndex::new(&layout);
        let portals = Self::portals_of(&layout);
        let active = self.window.as_ref().and_then(|w| w.portal.clone());
        let portal = active
            .as_ref()
            .and_then(|a| portals.iter().find(|p| p.object == a.object))
            .or_else(|| portals.first())
            .cloned()
            .ok_or(HostError::Unsupported)?;
        if self.window.as_ref().map(|w| w.occurrence) == Some(portal.occurrence) {
            if portal.own_order {
                return Err(HostError::Unsupported);
            }
            return om_scripts::runtime::Navigation::go_to_record(self, movement, false);
        }
        let (_, rows) = self.portal_rows(&portal)?;
        let current = active
            .as_ref()
            .filter(|a| a.object == portal.object)
            .map_or(0, |a| a.row);
        let missing = || Err(HostError::Error(ErrorCode::RECORD_MISSING));
        let row = match movement {
            _ if rows == 0 => return missing(),
            RecordMove::First => 1,
            RecordMove::Last => rows,
            RecordMove::Previous if current <= 1 => return Ok(false),
            RecordMove::Previous => current - 1,
            RecordMove::Next if current >= rows => return Ok(false),
            RecordMove::Next => current + 1,
            RecordMove::Number(n) if n < 1 || n as u64 > rows => return missing(),
            RecordMove::Number(n) => n as u64,
        };
        // Leaving the open record clears the focus: keep its object and
        // repetition when it is a field of this portal.
        let kept = active
            .filter(|active| active.object == portal.object)
            .and(self.focus.as_ref())
            .and_then(|focus| Some((focus.object_id?, focus.repetition)))
            .filter(|(id, _)| index.portal_of(*id) == Some(portal.object));
        self.leave()?;
        let (field, repetition) = if select {
            (None, 1)
        } else {
            match kept {
                Some((id, repetition)) => (index.get(id), repetition),
                None => (index.portal_fields(portal.object).first().copied(), 1),
            }
        };
        self.enter_row(&portal, row, field, repetition);
        Ok(true)
    }

    /// Go to Object (145): the object named `name` becomes active. A
    /// portal is active as a whole (its row kept, else row 0); a field in
    /// a portal is entered in the active row, else row 1 (101, the portal
    /// active, when it has no rows); another field is entered, the portal
    /// left. No such object is 116, an empty name 5, a repetition the
    /// field lacks 102; each leaves nothing active.
    pub(super) fn go_to_named_object(&mut self, name: &str, repetition: u32) -> HostResult {
        if name.is_empty() {
            self.leave_objects()?;
            return Err(ErrorCode::INVALID.into());
        }
        let Some(layout) = self.decoded_layout() else {
            return Err(OBJECT_MISSING.into());
        };
        let Some(object) = layout
            .objects
            .objects
            .iter()
            .find(|o| o.name.as_deref() == Some(name))
        else {
            self.leave_objects()?;
            return Err(OBJECT_MISSING.into());
        };
        let repetition = repetition.max(1);
        if let Some((occurrence, field)) = shown_field(object) {
            let repetitions = self
                .schema
                .field(occurrence, field)
                .map_or(1, |(_, def)| Self::repetitions(def));
            if repetition > repetitions {
                self.leave_objects()?;
                return Err(ErrorCode::FIELD_MISSING.into());
            }
        }
        let index = ObjectIndex::new(&layout);
        let portals = Self::portals_of(&layout);
        if object.kind() == ObjectKind::Portal {
            let portal = portals
                .iter()
                .find(|p| p.object == object.id)
                .cloned()
                .ok_or(HostError::Error(OBJECT_MISSING))?;
            let row = self.active_row_of(portal.object).unwrap_or(0);
            self.leave()?;
            self.enter_row(&portal, row, None, 1);
            return Ok(());
        }
        let holder = index
            .portal_of(object.id)
            .and_then(|id| portals.iter().find(|p| p.object == id))
            .cloned();
        match (holder, shown_field(object)) {
            (Some(portal), Some(_)) => self.enter_portal_field(&portal, object, repetition),
            (Some(_), None) | (None, None) => {
                // A button, text or another object: no field entered.
                self.leave_objects()?;
                Ok(())
            }
            (None, Some(_)) => {
                self.leave_objects()?;
                self.focus = focus_on(object, repetition);
                Ok(())
            }
        }
    }

    /// Enter `object`, a field of `portal`, in the active row of the
    /// portal, else row 1; 101 (the portal active) when it has no rows.
    fn enter_portal_field(
        &mut self,
        portal: &Portal,
        object: &LayoutObject,
        repetition: u32,
    ) -> HostResult {
        let (_, rows) = self.portal_rows(portal)?;
        let row = self
            .active_row_of(portal.object)
            .filter(|row| *row > 0)
            .unwrap_or(1);
        self.leave()?;
        if rows == 0 {
            self.enter_row(portal, 0, None, 1);
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        self.enter_row(portal, row, Some(object), repetition);
        Ok(())
    }

    /// Go to Field into `field`: in a portal on the layout, its first
    /// object is entered in the active row, else row 1, as Go to Object
    /// enters it (`Ok(true)`); `Ok(false)` for a field in no portal, whose
    /// entry is the caller's, the portal left.
    pub(super) fn go_to_portal_field(&mut self, field: FieldRef) -> HostResult<bool> {
        let Some(layout) = self.decoded_layout() else {
            self.clear_portal();
            return Ok(false);
        };
        let index = ObjectIndex::new(&layout);
        let portals = Self::portals_of(&layout);
        let found = layout.objects.objects.iter().find_map(|o| {
            let shown = shown_field(o)?;
            let portal = portals
                .iter()
                .find(|p| Some(p.object) == index.portal_of(o.id))?;
            (shown == (field.occurrence, field.field)).then(|| (portal.clone(), o))
        });
        let Some((portal, object)) = found else {
            self.clear_portal();
            return Ok(false);
        };
        let repetition = field.repetition.max(1);
        self.enter_portal_field(&portal, object, repetition)
            .map(|()| true)
    }

    /// The active row of portal `object`, when it is the active portal.
    fn active_row_of(&self, object: u64) -> Option<u64> {
        self.window
            .as_ref()
            .and_then(|w| w.portal.as_ref())
            .filter(|a| a.object == object)
            .map(|a| a.row)
    }

    /// No portal row is active any more.
    pub(super) fn clear_portal(&mut self) {
        if let Some(window) = self.window.as_mut() {
            window.portal = None;
        }
    }

    /// Leave every field and portal row.
    fn leave_objects(&mut self) -> HostResult {
        self.leave()?;
        self.clear_portal();
        Ok(())
    }

    /// Go to Record keeps the active row's number on the record it went
    /// to, and the field entered in it, `focus` (s35): a record without
    /// that row has none active, and the move is 101.
    pub(super) fn keep_portal_row(&mut self, focus: Option<Focus>) -> HostResult {
        let Some(active) = self.window.as_ref().and_then(|w| w.portal.clone()) else {
            return Ok(());
        };
        if active.row == 0 || self.window.as_ref().is_some_and(|w| w.mode != Mode::Browse) {
            return Ok(());
        }
        let portal = self
            .decoded_layout()
            .map(|layout| Self::portals_of(&layout))
            .and_then(|portals| portals.into_iter().find(|p| p.object == active.object));
        let rows = match &portal {
            Some(portal) => self.portal_rows(portal).map_or(0, |(_, rows)| rows),
            None => 0,
        };
        if rows < active.row {
            self.focus = None;
            self.clear_portal();
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        if focus
            .as_ref()
            .is_some_and(|f| f.occurrence == active.occurrence)
        {
            self.focus = focus;
        }
        Ok(())
    }
}

/// The focus of entering field object `object` in repetition `repetition`.
fn focus_on(object: &LayoutObject, repetition: u32) -> Option<Focus> {
    let (occurrence, field) = shown_field(object)?;
    Some(Focus {
        occurrence,
        field,
        object: object.name.clone().unwrap_or_default(),
        object_id: Some(object.id),
        repetition: repetition.max(1),
        typed: false,
    })
}
