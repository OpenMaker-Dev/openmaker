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
//! Record keeps its number on the next record, and leaving the layout or
//! committing the record clears it. A step's calculation reads the active
//! row's record for the portal's related fields ([`ServerHost::active_row_record`]);
//! field calculations read as before.
//!
//! Not yet: Set Field into a related field and Delete Portal Row (still
//! 3), and portals that sort or filter their own rows, whose rows are
//! refused (3) rather than numbered in an order that may not be
//! FileMaker's.

use om_calc::CalcError;
use om_data::meta::relation_option;
use om_layout::{Layout, LayoutObject, ObjectKind};
use om_scripts::runtime::{ErrorCode, HostError, HostResult, Mode, RecordMove};

use super::get::Focus;
use super::ServerHost;

/// "Layout object is missing" (Go to Object by a name no object has).
const OBJECT_MISSING: ErrorCode = ErrorCode(116);
/// The portal scrolls (`PortalLayoutObject.flags`, s35 `-word0`/`-word1`).
const PORTAL_SCROLLS: i32 = 0x1;
/// The portal sorts its rows by its stored sort order.
const PORTAL_SORTS: i32 = 0x8;

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
    let filters = calcs.portal_filter.is_some()
        || calcs.undecodable.contains(&om_layout::calcs::PORTAL_FILTER);
    Some(Portal {
        object: object.id,
        occurrence: om_layout::occurrence_id(def.table_alias_key)?,
        scrolls: def.flags & PORTAL_SCROLLS != 0,
        visible: (def.num_rows.max(1.0) as u64).max(1),
        own_order: sorts || filters,
        name: object.name.clone().unwrap_or_default(),
    })
}

/// The objects of `layout` inside portal `portal` that show a field, in
/// the order FileMaker enters them: top to bottom, then left to right.
fn portal_fields(layout: &Layout, portal: u64) -> Vec<&LayoutObject> {
    let inside = |object: &LayoutObject| {
        let mut at = object.parent;
        // A malformed store's parents could loop: no chain is longer
        // than the store.
        for _ in 0..=layout.objects.objects.len() {
            match at {
                Some(id) if id == portal => return true,
                Some(id) => at = layout.objects.get(id).and_then(|p| p.parent),
                None => return false,
            }
        }
        false
    };
    let mut fields: Vec<(f64, f64, &LayoutObject)> = layout
        .objects
        .objects
        .iter()
        .filter(|o| o.field_spec().is_some() && inside(o))
        .filter_map(|o| {
            let bounds = layout.absolute_bounds(o.id)?;
            Some((bounds.top, bounds.left, o))
        })
        .collect();
    fields.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    fields.into_iter().map(|(_, _, o)| o).collect()
}

/// The occurrence and field an object shows.
fn shown_field(object: &LayoutObject) -> Option<(u64, u64)> {
    let spec = object.field_spec()?;
    Some((
        om_layout::occurrence_id(spec.table_key)?,
        om_hbam::serial::raw_of_id(spec.field_key)?,
    ))
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

    /// The portals of the window's layout in stacking order, back-most
    /// first (the "first portal in the layout stacking order" Go to Portal
    /// Row uses when none is active).
    fn portals(&self) -> Vec<Portal> {
        let Some(layout) = self.decoded_layout() else {
            return Vec::new();
        };
        om_data::host::layout_objects::objects_in_order(&layout)
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

    /// The record a step's calculation reads for a related field of
    /// `related` from record `record` of `occurrence`, when the window's
    /// active portal row is one of those records' rows: `Some(Some(id))`
    /// its record, `Some(None)` the creation row (an empty record), `None`
    /// no active row there (the first related record is read, as before).
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
    /// portal) entered, or none.
    fn enter_row(&mut self, portal: &Portal, row: u64, field: Option<&LayoutObject>) {
        let focus = field.and_then(|object| {
            let (occurrence, field) = shown_field(object)?;
            Some(Focus {
                occurrence,
                field,
                object: object.name.clone().unwrap_or_default(),
                typed: false,
            })
        });
        self.focus = focus;
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
    /// step keeps the field entered in the portal, else enters the row's
    /// first field; with Select the row is active without a field. A
    /// portal of the layout's own occurrence moves the found set.
    pub(super) fn go_to_portal_row(
        &mut self,
        movement: RecordMove,
        select: bool,
    ) -> HostResult<bool> {
        let active = self.window.as_ref().and_then(|w| w.portal.clone());
        let portals = self.portals();
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
        // Leaving the open record clears focus, so remember it first.
        let kept = active
            .filter(|active| active.object == portal.object)
            .and_then(|_| self.focus.as_ref().map(|focus| focus.object.clone()));
        self.leave()?;
        let layout = self.decoded_layout();
        let fields = layout
            .as_deref()
            .map(|l| portal_fields(l, portal.object))
            .unwrap_or_default();
        // Keep the previously entered field in this portal, or enter its first.
        let field = if select {
            None
        } else {
            kept.and_then(|name| {
                fields
                    .iter()
                    .find(|o| !name.is_empty() && o.name.as_deref() == Some(name.as_str()))
                    .copied()
            })
            .or_else(|| fields.first().copied())
        };
        self.enter_row(&portal, row, field);
        Ok(true)
    }

    /// Go to Object (145): the object named `name` becomes active. A
    /// portal is active as a whole (its row kept, else row 0); a field in
    /// a portal is entered in the active row, else row 1 (101, the portal
    /// active, when it has no rows); another field is entered. No such
    /// object is 116, an empty name 5, a repetition the field lacks 102;
    /// each leaves nothing active.
    pub(super) fn go_to_named_object(&mut self, name: &str, repetition: u32) -> HostResult {
        if name.is_empty() {
            self.leave_objects()?;
            return Err(ErrorCode::INVALID.into());
        }
        let Some(layout) = self.decoded_layout() else {
            return Err(OBJECT_MISSING.into());
        };
        let objects = om_data::host::layout_objects::objects_in_order(&layout);
        let Some(object) = objects
            .iter()
            .find(|o| o.name.as_deref() == Some(name))
            .copied()
        else {
            self.leave_objects()?;
            return Err(OBJECT_MISSING.into());
        };
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
        let portals = self.portals();
        if object.kind() == ObjectKind::Portal {
            let portal = portals
                .iter()
                .find(|p| p.object == object.id)
                .cloned()
                .ok_or(HostError::Error(OBJECT_MISSING))?;
            let row = self
                .window
                .as_ref()
                .and_then(|w| w.portal.as_ref())
                .filter(|a| a.object == portal.object)
                .map_or(0, |a| a.row);
            self.leave()?;
            self.enter_row(&portal, row, None);
            return Ok(());
        }
        let holder = portals
            .iter()
            .find(|p| {
                portal_fields(&layout, p.object)
                    .iter()
                    .any(|f| f.id == object.id)
            })
            .cloned();
        if let Some(portal) = holder {
            let (_, rows) = self.portal_rows(&portal)?;
            let row = self
                .window
                .as_ref()
                .and_then(|w| w.portal.as_ref())
                .filter(|a| a.object == portal.object && a.row > 0)
                .map_or(1, |a| a.row);
            self.leave()?;
            if rows == 0 {
                self.enter_row(&portal, 0, None);
                return Err(ErrorCode::RECORD_MISSING.into());
            }
            self.enter_row(&portal, row, Some(object));
            return Ok(());
        }
        let Some((occurrence, field)) = shown_field(object) else {
            // A button, text or another object: active, no field.
            self.leave_objects()?;
            return Ok(());
        };
        self.leave_objects()?;
        self.focus = Some(Focus {
            occurrence,
            field,
            object: name.to_owned(),
            typed: false,
        });
        Ok(())
    }

    /// Leave every field and portal row.
    fn leave_objects(&mut self) -> HostResult {
        self.leave()?;
        if let Some(window) = self.window.as_mut() {
            window.portal = None;
        }
        Ok(())
    }

    /// Go to Record keeps the active row's number on the record it went
    /// to (s35): a record without that row has none active, and the move
    /// is 101.
    pub(super) fn keep_portal_row(&mut self) -> HostResult {
        let Some(active) = self.window.as_ref().and_then(|w| w.portal.clone()) else {
            return Ok(());
        };
        if active.row == 0 || self.window.as_ref().is_some_and(|w| w.mode != Mode::Browse) {
            return Ok(());
        }
        let portal = self
            .portals()
            .into_iter()
            .find(|p| p.object == active.object);
        let rows = match &portal {
            Some(portal) => self.portal_rows(portal).map_or(0, |(_, rows)| rows),
            None => 0,
        };
        if rows < active.row {
            self.focus = None;
            if let Some(window) = self.window.as_mut() {
                window.portal = None;
            }
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        Ok(())
    }
}
