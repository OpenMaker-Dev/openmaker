//! Moving, resizing and deleting a layout's stored objects in Layout mode
//! (docs/re/layout-model.md 10.7, 10.8), as edits of the layout member
//! `[4, 5, L]` as stored: only the records that hold the objects change.
//!
//! A move or resize ([`plan`]) changes:
//!
//! - the object's `bounds` in the object store `07`, rewritten in place
//!   (`om_layout::encode::with_object_bounds`): every other byte of the
//!   store stays as it is;
//! - its entry in the object index `0a`, whose key is the object's top and
//!   left and its place in the object tree: the entry is keyed again at the
//!   new position, its place and value kept.
//!
//! A deletion ([`plan_deletion`]) removes every record of the object:
//!
//! - its definition and root key in the object store
//!   (`om_layout::encode::without_objects`), every other byte kept;
//! - its attribute record `05, <object>`, its entries in the field
//!   reference index `01`, the named objects `08` and the tab order `15`,
//!   and its entry in its part's list `03, <part>, 07`;
//! - its entry in the object index `0a`, the entries after it in the object
//!   tree renumbered to their new places.
//!
//! An object that shows an image from the file's library is not deleted:
//! FileMaker counts each use of a library item and removes the item with its
//! last use (layout-model.md 10.8), which this version does not write. Nor
//! is an object another one names by key (a field's label) or a popover
//! button, whose popover would stay without one. The
//! last object key issued (`05, 00`) stays, so a key is not issued
//! again. OpenMaker Server's authoring extension adds the layout's stamp
//! and saves the result; clients plan with the same code, so a plan names
//! the edit the host makes.
//!
//! Only top-level objects without children are moved or deleted (fields
//! showing one repetition, text, shapes, lines, buttons, pictures); a move
//! stays within the part whose list names the object and the layout's
//! width, at whole points, and not over a container (a group, portal, tab
//! or slide control, popover or button bar), which FileMaker Pro would make
//! it part of.

use std::collections::{BTreeSet, HashMap, HashSet};

use om_hbam::slot::be_uint;
use om_hbam::{serial, OwnedNode};
use om_layout::wire::{Fields, RawValue, Value};
use om_layout::{LayoutDef, LayoutObject, ObjectKind, Rect};

use crate::layout_placement::{
    definition, frame_text, hex, is_container, key_string, kind_name, overlap, part_list, parts,
    preorder, stored_objects, stored_value, Refusal, ATTRIBUTES, FIELD_INDEX, MAX_COORDINATE,
    MEMBER_FLAGS, NOT_A_LAYOUT, OBJECTS, OBJECT_INDEX, PARTS, PART_OBJECTS, RESPONSIVE,
};

/// The most objects one request moves or deletes.
pub const MAX_FRAMES: usize = 1_000;

/// Keys of a layout member `[4, 5, L]` (layout-model.md 10.5): the named
/// objects and the tab order.
const NAMED: u8 = 0x08;
/// The layout's stored Table View layout (layout-model.md 10.14).
const TABLE_VIEW: u8 = 0x0d;
const TAB_ORDER: u8 = 0x15;

/// The deepest nesting of the object index OpenMaker walks.
const MAX_INDEX_DEPTH: usize = 64;

/// An object's new frame: the object's key (as the layout's objects and
/// references give it) and its frame in layout points.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectFrame {
    pub object: u64,
    pub frame: Rect,
}

/// Objects of layout `layout` moved or resized in Layout mode, as the window
/// asks for it ([`crate::DataProvider::set_layout_object_frames`]): the
/// frames, and the revision of the layout the window showed them on.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutObjectFrames {
    pub layout: u32,
    pub frames: Vec<ObjectFrame>,
    pub expected: crate::layout_placement::Revision,
}

/// Frames the host set, or found set by a request whose answer was lost
/// (`changed` false), and the layout's revision as stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FramedObjects {
    pub changed: bool,
    pub revision: crate::layout_placement::Revision,
}

/// Objects of layout `layout` deleted in Layout mode, as the window asks
/// for it ([`crate::DataProvider::delete_layout_objects`]): the objects'
/// keys, and the revision of the layout the window showed them on.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutObjectDeletion {
    pub layout: u32,
    pub objects: Vec<u64>,
    pub expected: crate::layout_placement::Revision,
}

/// Objects the host deleted, or found deleted by a request whose answer was
/// lost (`changed` false), and the layout's revision as stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeletedObjects {
    pub changed: bool,
    pub revision: crate::layout_placement::Revision,
}

/// Frames or a deletion planned into a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    /// The member `[4, 5, L]` with the objects at their new frames, or
    /// without the deleted objects, without the stamp.
    pub member: OwnedNode,
    /// What changed: each changed key below the member (as hex segments)
    /// and what it holds.
    pub changes: Vec<(String, String)>,
}

/// The field controls a field object can be given (layout-model.md 5.4),
/// by display type, with the style type FileMaker writes for each: 61 Edit
/// Box, 62 Drop-down List, 63 Pop-up Menu, 64 Checkbox Set, 65 Radio Button
/// Set (from FMUpgradeTool's own write alone: no starter holds one), 66
/// Drop-down Calendar.
pub const FIELD_CONTROLS: [(u32, u64, &str); 6] = [
    (0, 61, "Edit Box"),
    (1, 62, "Drop-down List"),
    (2, 63, "Pop-up Menu"),
    (3, 64, "Checkbox Set"),
    (4, 65, "Radio Button Set"),
    (6, 66, "Drop-down Calendar"),
];

/// A field object's new control, as the window or `om-author` asks for it
/// ([`crate::DataProvider::set_field_control`]): object `object` of layout
/// `layout` shown at revision `expected`, display type `control`, and the
/// value list (`None` keeps the stored one, `Some(0)` clears it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldControlChange {
    pub layout: u32,
    pub expected: crate::layout_placement::Revision,
    pub object: u64,
    pub control: u32,
    pub value_list: Option<u64>,
}

/// What the host knows of the object's field and the value list: the
/// field's spec (its table and field keys, as the object stores them), whether
/// it is a date field and whether it is a container, and whether the value
/// list the object will show (the one asked for, or else the one it stores)
/// exists in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlFacts {
    pub field: (u64, u64),
    pub date: bool,
    pub container: bool,
    pub value_list_exists: bool,
}

/// Plan object `object` of layout `member` showing its field as `control`
/// (a display type of [`FIELD_CONTROLS`]) with value list `value_list`
/// (`None`: as stored): the field object's display type and value list and
/// its style's type rewritten in place
/// (`om_layout::encode::with_field_control`), its flags kept as FileMaker
/// Pro's Inspector keeps them and its custom style dropped so that it takes
/// the new type's default from the file's theme (layout-model.md 10.13).
pub fn plan_control(
    member: &OwnedNode,
    object: u64,
    (control, value_list): (u32, Option<u64>),
    facts: &ControlFacts,
) -> Result<Planned, Refusal> {
    let invalid = |why: String| Refusal::Invalid(why);
    let unsupported = |why: String| Refusal::Unsupported(why);
    let (_, style_type, name) = *FIELD_CONTROLS
        .iter()
        .find(|(display, ..)| *display == control)
        .ok_or_else(|| {
            invalid(format!(
                "Display type {control} is not a control OpenMaker sets."
            ))
        })?;
    let (stored, store) = stored_objects(member)?;
    let found = store
        .objects
        .iter()
        .find(|o| o.id == object)
        .ok_or_else(|| invalid(format!("The layout has no object {object}.")))?;
    let field = found
        .field_obj
        .as_ref()
        .filter(|_| found.object_type == 1)
        .ok_or_else(|| invalid(format!("Object {object} is not a field.")))?;
    let spec = field
        .field_spec
        .as_ref()
        .map(|s| (s.table_key, s.field_key));
    if spec != Some(facts.field) {
        return Err(unsupported(format!(
            "Object {object} shows another field than the one checked; read the layout again."
        )));
    }
    let current = found.style.as_ref().map(|s| s.object_type_id);
    if facts.container || current == Some(67) {
        return Err(unsupported("A container field keeps its control.".into()));
    }
    if field.num_of_reps > 1 {
        return Err(unsupported(
            "A field showing several repetitions keeps its control in this version.".into(),
        ));
    }
    if !FIELD_CONTROLS
        .iter()
        .any(|(d, ..)| *d == field.display_type)
    {
        return Err(unsupported(format!(
            "Object {object}'s control (display type {}) is not one OpenMaker changes.",
            field.display_type
        )));
    }
    let list = value_list.unwrap_or(field.value_list_key);
    if (1..=4).contains(&control) && list == 0 {
        return Err(invalid(format!("A {name} shows a value list: name one.")));
    }
    // A list control needs its list, and a list named in the request must
    // exist; an edit box may keep the key of a list the file lost.
    if list != 0 && !facts.value_list_exists && ((1..=4).contains(&control) || value_list.is_some())
    {
        return Err(invalid("The file has no such value list.".into()));
    }
    if control == 6 && !facts.date {
        return Err(invalid("A drop-down calendar shows a date field.".into()));
    }
    let flags = field.flags;
    let bytes = om_layout::encode::with_field_control(
        &stored,
        object,
        &om_layout::encode::FieldControl {
            flags: u64::from(flags),
            display_type: u64::from(control),
            value_list_key: list,
            style_type,
        },
    )
    .ok_or_else(|| {
        unsupported(format!(
            "Object {object} is stored in a form OpenMaker does not change."
        ))
    })?;
    let mut next = member.clone();
    next.set_child(stored_value(&[OBJECTS], &bytes));
    Ok(Planned {
        member: next,
        changes: vec![(
            "[07]".into(),
            format!("object {object} shown as a {name}, value list {list}"),
        )],
    })
}

/// Whether object `object` of layout `member` already shows its field as
/// `control` with value list `value_list` (`None`: any): a request whose
/// answer was lost.
pub fn already_control(
    member: &OwnedNode,
    object: u64,
    control: u32,
    value_list: Option<u64>,
) -> bool {
    let Ok((_, store)) = stored_objects(member) else {
        return false;
    };
    let Some((_, style_type, _)) = FIELD_CONTROLS.iter().find(|(d, ..)| *d == control) else {
        return false;
    };
    store.objects.iter().any(|o| {
        o.id == object
            && o.style
                .as_ref()
                .is_some_and(|s| s.object_type_id as u64 == *style_type)
            && o.field_obj.as_ref().is_some_and(|f| {
                f.display_type == control && value_list.is_none_or(|key| f.value_list_key == key)
            })
    })
}

/// Whether object `object` of a layout's `store` can be moved, resized or
/// deleted: a top-level object without children, not a container, a field
/// showing one repetition.
pub fn movable(store: &om_layout::ObjectStore, object: u64) -> Result<&LayoutObject, Refusal> {
    simple(store, object, "moves")
}

/// Whether object `object` of a layout's `store` can be deleted: a
/// [`movable`] one that no other object names by key ([`named_by`]), is
/// not a popover button (its popover would stay without one) and shows no
/// image from the file's library ([`uses_library`]).
pub fn deletable(store: &om_layout::ObjectStore, object: u64) -> Result<&LayoutObject, Refusal> {
    deletable_with(store, object, &[])
}

/// [`deletable`], when objects `with` are deleted together with it: their
/// keys naming it do not count.
fn deletable_with<'a>(
    store: &'a om_layout::ObjectStore,
    object: u64,
    with: &[u64],
) -> Result<&'a LayoutObject, Refusal> {
    let found = simple(store, object, "deletes")?;
    if found.kind() == ObjectKind::PopoverButton {
        return Err(Refusal::Unsupported(format!(
            "Object {object} is a popover button; this version does not delete it, as its \
             popover would stay without one."
        )));
    }
    if let Some(other) = named_by(store, object, with) {
        return Err(Refusal::Unsupported(format!(
            "Object {} (a {}) names object {object}, as a field names its label; this version \
             does not delete an object another one names.",
            other.id,
            kind_name(other.kind())
        )));
    }
    if uses_library(found) {
        return Err(Refusal::Unsupported(format!(
            "Object {object} shows an image from the file's library; FileMaker counts each use \
             of a library image, and this version does not delete such objects yet."
        )));
    }
    Ok(found)
}

/// An object of `store` other than `object` and `with` that names `object`
/// by key (layout-model.md 5.2, 10.8): a field's `labelKey` (its label), a
/// popover's `buttonKey`, a popover button's `popoverKey`, a tab control's
/// default or current panel, or a button bar's active segment.
pub fn named_by<'a>(
    store: &'a om_layout::ObjectStore,
    object: u64,
    with: &[u64],
) -> Option<&'a LayoutObject> {
    store.objects.iter().find(|other| {
        other.id != object
            && !with.contains(&other.id)
            && [
                other.label_key,
                other.popover.as_ref().map_or(0, |p| p.button_key),
                other.popover_button.as_ref().map_or(0, |p| p.popover_key),
                other.tab.as_ref().map_or(0, |t| t.default_panel_key),
                other.tab.as_ref().map_or(0, |t| t.current_panel_key),
                other
                    .segmented_bar
                    .as_ref()
                    .map_or(0, |s| s.active_segment_key),
            ]
            .contains(&object)
    })
}

/// Whether `object` uses an item of the file's library `[31, 5]`
/// (library-images.md): a button's, popover button's or graphic's
/// `libraryKey` ([`library_keys`]), or an image in its own styles
/// ([`style_image`]).
pub fn uses_library(object: &LayoutObject) -> bool {
    library_keys(object).next().is_some()
        || object
            .style
            .as_ref()
            .and_then(|s| s.local_styles.as_ref())
            .is_some_and(style_image)
}

/// The library items `object` shows as a button's, popover button's or
/// graphic's `libraryKey`: the uses FileMaker counts (layout-model.md 10.8).
pub fn library_keys(object: &LayoutObject) -> impl Iterator<Item = u64> {
    [
        object.button.as_ref().map(|b| b.library_key),
        object.popover_button.as_ref().map(|b| b.library_key),
        object.graphic.as_ref().map(|g| g.library_key),
    ]
    .into_iter()
    .flatten()
    .filter(|key| *key != 0)
}

/// Whether `styles` holds an image from the file's library: a property's
/// `imageVal` 12 or `backgroundImageVal` 18, an `Image` whose `libraryKey`
/// is field 3. An image that does not read counts as one.
pub fn style_image(styles: &om_layout::StyleSet) -> bool {
    styles
        .components
        .iter()
        .flat_map(|component| &component.properties)
        .flat_map(|property| &property.other)
        .any(|field| match (field.number, &field.value) {
            (12 | 18, RawValue::Bytes(image)) => {
                Fields::new(image, "Image").any(|field| match field {
                    Ok(field) => field.number == 3 && field.value != Value::Varint(0),
                    Err(_) => true,
                })
            }
            (12 | 18, _) => true,
            _ => false,
        })
}

/// [`movable`], `verb` naming the edit in a refusal ("moves", "deletes").
fn simple<'a>(
    store: &'a om_layout::ObjectStore,
    object: u64,
    verb: &str,
) -> Result<&'a LayoutObject, Refusal> {
    let unsupported = |why: String| Err(Refusal::Unsupported(why));
    let Some(found) = store.objects.iter().find(|o| o.id == object) else {
        return Err(Refusal::Invalid(format!(
            "The layout has no object {object}."
        )));
    };
    if found.is_locked() {
        return unsupported(format!("Object {object} is locked in Layout mode."));
    }
    if !store.root.contains(&object) {
        return unsupported(format!(
            "Object {object} is inside another object; this version {verb} top-level objects \
             only."
        ));
    }
    if is_container(found.kind()) || !found.children.is_empty() {
        return unsupported(format!(
            "Object {object} is a {} that holds other objects; this version {verb} objects \
             without contents only.",
            kind_name(found.kind())
        ));
    }
    if found.kind() == ObjectKind::Field
        && found.field_obj.as_ref().is_some_and(|f| f.num_of_reps > 1)
    {
        return unsupported(format!(
            "Object {object} shows several repetitions; this version {verb} fields showing one \
             repetition only."
        ));
    }
    Ok(found)
}

/// Check an object's new frame: finite, not negative, at whole points top
/// and left (the object index holds whole points, layout-model.md 10.5), not
/// both empty (a line is empty one way), within [`MAX_COORDINATE`] and the
/// layout's width.
fn check_frame(frame: &Rect, width: f64, object: u64) -> Result<(), Refusal> {
    let invalid = |why: String| {
        Err(Refusal::Invalid(format!(
            "Object {object}'s new frame ({}) {why}.",
            frame_text(frame)
        )))
    };
    let edges = [frame.top, frame.left, frame.bottom, frame.right];
    if edges.iter().any(|e| !e.is_finite()) {
        return invalid("has an edge that is not a number".into());
    }
    if frame.top < 0.0 || frame.left < 0.0 {
        return invalid("starts above or left of the layout".into());
    }
    if frame.top.fract() != 0.0 || frame.left.fract() != 0.0 {
        return invalid("starts between whole points".into());
    }
    if frame.right < frame.left
        || frame.bottom < frame.top
        || (frame.right == frame.left && frame.bottom == frame.top)
    {
        return invalid("has no size".into());
    }
    if frame.right > MAX_COORDINATE || frame.bottom > MAX_COORDINATE {
        return invalid(format!("reaches past {MAX_COORDINATE} points"));
    }
    if width > 0.0 && frame.right > width {
        return invalid(format!(
            "reaches past the layout's width of {width} points, where Browse does not show it"
        ));
    }
    Ok(())
}

/// The part whose list `[4, 5, L, 03, P, 07]` names `object`, by its id.
fn listing_part(member: &OwnedNode, object: u64) -> Option<u64> {
    member.child(&[PARTS])?.children.iter().find_map(|part| {
        let list = part.child(&[PART_OBJECTS])?.value.as_deref()?;
        part_list(list)?
            .contains(&object)
            .then(|| serial::decode(&part.segment))
            .flatten()
    })
}

/// Whether every object of `frames` is at its frame in `member`: a request
/// already made, whose answer was lost.
pub fn already_framed(member: &OwnedNode, frames: &[ObjectFrame]) -> bool {
    let Ok((_, store)) = stored_objects(member) else {
        return false;
    };
    !frames.is_empty()
        && frames.iter().all(|f| {
            // The four edges: stored bounds may keep unknown fields.
            let edges = |r: &Rect| [r.top, r.left, r.bottom, r.right];
            store
                .objects
                .iter()
                .any(|o| o.id == f.object && o.bounds.as_ref().map(edges) == Some(edges(&f.frame)))
        })
}

/// The definition of `member`, a layout member `[4, 5, L]`, if it is a
/// fixed layout, whose objects OpenMaker changes.
fn fixed_layout(member: &OwnedNode) -> Result<LayoutDef, Refusal> {
    let flags = member
        .child(&[MEMBER_FLAGS])
        .and_then(|n| n.value.as_deref())
        .and_then(be_uint)
        .unwrap_or(0);
    if flags & NOT_A_LAYOUT != 0 {
        return Err(Refusal::Unsupported(
            "This member of the layout catalog is a folder or a separator, not a layout.".into(),
        ));
    }
    let def = definition(member)?;
    if def.layout_client_type == RESPONSIVE {
        return Err(Refusal::Unsupported(
            "The layout is responsive; OpenMaker changes objects on fixed layouts only.".into(),
        ));
    }
    Ok(def)
}

/// Plan `frames` into `member`, a layout member `[4, 5, L]` as stored.
pub fn plan(member: &OwnedNode, frames: &[ObjectFrame]) -> Result<Planned, Refusal> {
    let unsupported = |why: String| Refusal::Unsupported(why);
    if frames.is_empty() || frames.len() > MAX_FRAMES {
        return Err(Refusal::Invalid(format!(
            "A request moves 1 to {MAX_FRAMES} objects."
        )));
    }
    let def = fixed_layout(member)?;
    let parts = parts(member)?;
    let (stored, store) = stored_objects(member)?;
    let geometry = om_layout::Layout {
        objects: store.clone(),
        ..Default::default()
    };
    let moved: Vec<u64> = frames.iter().map(|f| f.object).collect();
    let mut bytes = stored;
    let mut index = member.child(&[OBJECT_INDEX]).cloned();
    let mut changes = Vec::new();
    for (i, request) in frames.iter().enumerate() {
        if moved[..i].contains(&request.object) {
            return Err(Refusal::Invalid(format!(
                "Object {} is named twice.",
                request.object
            )));
        }
        let object = movable(&store, request.object)?;
        let frame = &request.frame;
        check_frame(frame, def.base_width, object.id)?;
        let part_id = listing_part(member, object.id).ok_or_else(|| {
            unsupported(format!(
                "No part of the layout lists object {}; OpenMaker moves objects within the part \
                 that lists them.",
                object.id
            ))
        })?;
        let part = parts.iter().find(|p| p.id == part_id).ok_or_else(|| {
            unsupported(format!(
                "OpenMaker cannot read part {part_id} of the layout."
            ))
        })?;
        if frame.top < part.top || frame.bottom > part.bottom() {
            return Err(Refusal::Invalid(format!(
                "Object {}'s new frame ({}) is not inside its part, which runs from {} to {} \
                 points down the layout; this version moves objects within their part.",
                object.id,
                frame_text(frame),
                part.top,
                part.bottom()
            )));
        }
        // Not in or over a container, which would take the object in.
        for other in &store.objects {
            if moved.contains(&other.id) || !is_container(other.kind()) {
                continue;
            }
            let Some(bounds) = geometry.absolute_bounds(other.id) else {
                continue;
            };
            if overlap(frame, &bounds) {
                return Err(unsupported(format!(
                    "Object {}'s new frame overlaps object {} (a {}); this version does not move \
                     objects in or over groups, portals, tab and slide controls, popovers or \
                     button bars.",
                    object.id,
                    other.id,
                    kind_name(other.kind())
                )));
            }
        }
        bytes =
            om_layout::encode::with_object_bounds(&bytes, object.id, frame).ok_or_else(|| {
                unsupported(format!(
                    "OpenMaker cannot rewrite object {}'s frame in the stored object store.",
                    object.id
                ))
            })?;
        changes.push((
            format!("[04, 05, L, 07] object {}", object.id),
            format!("bounds {}", frame_text(frame)),
        ));
        if let Some(index) = index.as_mut() {
            rekey(index, object.id, frame, &mut changes)?;
        }
    }
    let mut next = member.clone();
    next.set_child(stored_value(&[OBJECTS], &bytes));
    if let Some(index) = index {
        next.set_child(index);
    }
    Ok(Planned {
        member: next,
        changes,
    })
}

/// Key `object`'s top-level entry in the object index `0a` at `frame`'s
/// top and left, keeping its place in the object tree and its value. An
/// object the index does not list keeps none; one listed more than once,
/// or with entries below it, is refused.
fn rekey(
    index: &mut OwnedNode,
    object: u64,
    frame: &Rect,
    changes: &mut Vec<(String, String)>,
) -> Result<(), Refusal> {
    let key = key_string(object);
    let mut prefix = vec![0, key.len() as u8];
    prefix.extend(&key);
    let listed: Vec<usize> = index
        .children
        .iter()
        .enumerate()
        .filter(|(_, e)| e.value.as_deref().is_some_and(|v| v.starts_with(&prefix)))
        .map(|(i, _)| i)
        .collect();
    let at = match listed.as_slice() {
        [] => return Ok(()),
        [at] => *at,
        _ => {
            return Err(Refusal::Unsupported(format!(
                "The layout's object index lists object {object} more than once."
            )))
        }
    };
    let entry = &index.children[at];
    if !entry.children.is_empty() || entry.segment.len() != 6 {
        return Err(Refusal::Unsupported(format!(
            "The layout's object index entry for object {object} is not one OpenMaker reads."
        )));
    }
    let sequence = [entry.segment[4], entry.segment[5]];
    let mut segment = Vec::with_capacity(6);
    segment.extend((frame.top as u16).to_be_bytes());
    segment.extend((frame.left as u16).to_be_bytes());
    segment.extend(sequence);
    if segment == entry.segment {
        return Ok(());
    }
    if index.child(&segment).is_some() {
        return Err(Refusal::Unsupported(format!(
            "The layout's object index already has an entry at object {object}'s new position."
        )));
    }
    let mut moved = index.children.remove(at);
    changes.push((
        "[04, 05, L, 0a]".into(),
        format!(
            "object {object}'s entry from {} to {}",
            hex(&moved.segment),
            hex(&segment)
        ),
    ));
    moved.segment = segment;
    index.set_child(moved);
    Ok(())
}

/// Whether none of `objects` is in `member`'s object store: a deletion
/// already made, whose answer was lost. Object keys are not issued again
/// (`05, 00` stays), so a deleted key does not come back.
pub fn already_deleted(member: &OwnedNode, objects: &[u64]) -> bool {
    let Ok((_, store)) = stored_objects(member) else {
        return false;
    };
    !objects.is_empty()
        && objects
            .iter()
            .all(|object| !store.objects.iter().any(|o| o.id == *object))
}

/// Plan deleting `objects` from `member`, a layout member `[4, 5, L]` as
/// stored (layout-model.md 10.8).
pub fn plan_deletion(member: &OwnedNode, objects: &[u64]) -> Result<Planned, Refusal> {
    let unsupported = |why: &str| Refusal::Unsupported(why.into());
    if objects.is_empty() || objects.len() > MAX_FRAMES {
        return Err(Refusal::Invalid(format!(
            "A request deletes 1 to {MAX_FRAMES} objects."
        )));
    }
    let def = fixed_layout(member)?;
    let (stored, store) = stored_objects(member)?;
    for (i, &object) in objects.iter().enumerate() {
        if objects[..i].contains(&object) {
            return Err(Refusal::Invalid(format!("Object {object} is named twice.")));
        }
        deletable_with(&store, object, objects)?;
    }
    let order = preorder(&store)?;
    let bytes = om_layout::encode::without_objects(&stored, objects).ok_or_else(|| {
        unsupported("OpenMaker cannot remove the objects from the stored object store.")
    })?;
    let mut next = member.clone();
    let mut changes = Vec::new();
    next.set_child(stored_value(&[OBJECTS], &bytes));
    for object in objects {
        changes.push(("[04, 05, L, 07]".into(), format!("object {object} removed")));
        let record = serial::encode(*object);
        if next.remove(&[&[ATTRIBUTES], &record]).is_some() {
            changes.push((
                format!("[04, 05, L, 05, {}]", hex(&record)),
                format!("object {object}'s attributes removed"),
            ));
        }
    }
    unreference(&mut next, objects, def.table_alias_key, &mut changes);
    unlist(&mut next, objects, &mut changes)?;
    if let Some(index) = next.child(&[OBJECT_INDEX]) {
        let mut index = index.clone();
        if order.len() > usize::from(u16::MAX) {
            return Err(unsupported(
                "The layout has more objects than its object index numbers.",
            ));
        }
        let after: Vec<u64> = order
            .iter()
            .copied()
            .filter(|o| !objects.contains(o))
            .collect();
        let place = |order: &[u64]| -> HashMap<u64, u16> {
            // Below u16::MAX entries, checked above.
            order
                .iter()
                .enumerate()
                .map(|(i, o)| (*o, i as u16))
                .collect()
        };
        let renumbering = Renumbering {
            deleted: objects,
            order: &order,
            before: place(&order),
            after: place(&after),
            containers: store
                .objects
                .iter()
                .filter(|o| is_container(o.kind()))
                .map(|o| o.id)
                .collect(),
        };
        let mut renumbered = 0;
        renumbering.apply(&mut index, 0, &mut renumbered)?;
        for object in objects {
            if !crate::layout_placement::indexes(member, *object) {
                continue;
            }
            changes.push((
                "[04, 05, L, 0a]".into(),
                format!("object {object}'s entry removed"),
            ));
        }
        if renumbered > 0 {
            changes.push((
                "[04, 05, L, 0a]".into(),
                format!("{renumbered} entries renumbered to their objects' new places"),
            ));
        }
        next.set_child(index);
    }
    for (key, what) in [(NAMED, "named objects"), (TAB_ORDER, "tab order")] {
        let Some(node) = next.child(&[key]) else {
            continue;
        };
        let mut node = node.clone();
        let before = node.children.len();
        node.children.retain(|entry| {
            !entry
                .value
                .as_deref()
                .is_some_and(|v| objects.iter().any(|o| names(v, *o, key == NAMED)))
        });
        if node.children.len() != before {
            changes.push((
                format!("[04, 05, L, {key:02x}]"),
                format!(
                    "{} entries of the {what} removed",
                    before - node.children.len()
                ),
            ));
            next.set_child(node);
        }
    }
    table_view_after_deletion(
        &mut next,
        &store,
        objects,
        def.table_alias_key,
        &mut changes,
    )?;
    Ok(Planned {
        member: next,
        changes,
    })
}

/// The stored Table View layout `[4, 5, L, 0d]` after `objects` of `store`
/// are deleted from `member` (layout-model.md 10.14, F88, #814): the
/// columns of the fields no field object shows any more are taken out and
/// the empty column renewed, as FileMaker Pro 26.0.3 does; a layout left
/// with no field object and no stored table view gets the one a new layout
/// stores, without which FileMaker Pro quits when it is shown as a table. A
/// table view OpenMaker does not change is kept as stored: its columns
/// still show fields the table has.
fn table_view_after_deletion(
    member: &mut OwnedNode,
    store: &om_layout::ObjectStore,
    objects: &[u64],
    alias: u64,
    changes: &mut Vec<(String, String)>,
) -> Result<(), Refusal> {
    let shown = |deleted: bool| -> BTreeSet<(u64, u64)> {
        store
            .objects
            .iter()
            .filter(|o| o.kind() == ObjectKind::Field && objects.contains(&o.id) == deleted)
            .filter_map(|o| o.field_spec().map(|s| (s.table_key, s.field_key)))
            .collect()
    };
    let (kept, gone) = (shown(false), shown(true));
    let fields_left = store
        .objects
        .iter()
        .any(|o| o.kind() == ObjectKind::Field && !objects.contains(&o.id));
    match member.child(&[TABLE_VIEW]) {
        Some(view) => {
            let unshown: BTreeSet<(u64, u64)> = gone.difference(&kept).copied().collect();
            // A table view without a column (an empty object store) is
            // given one once no field object is left.
            let columnless = stored_objects(view).is_ok_and(|(_, s)| s.objects.is_empty());
            if unshown.is_empty() && !(columnless && !fields_left) {
                return Ok(());
            }
            match crate::layout_placement::without_table_columns(view, &unshown) {
                Ok(view) => {
                    changes.push((
                        "[04, 05, L, 0d]".into(),
                        format!(
                            "the stored table view: the columns of {} field(s) no object shows \
                             any more taken out, and the empty column renewed",
                            unshown.len()
                        ),
                    ));
                    member.set_child(view);
                }
                // With no field object left, a table view kept without a
                // column is one FileMaker Pro quits on (10.14): refused.
                Err(why) if !fields_left => return Err(why),
                Err(why) => changes.push((
                    "[04, 05, L, 0d]".into(),
                    format!("the stored table view kept as stored: {why}"),
                )),
            }
        }
        None if !fields_left => {
            let occurrence = be_uint(&serial::encode(alias)).ok_or_else(|| {
                Refusal::Unsupported("The layout's table occurrence key does not read.".into())
            })?;
            let view = crate::layout_catalog::table_view(occurrence)
                .map_err(|e| Refusal::Unsupported(e.to_string()))?;
            changes.push((
                "[04, 05, L, 0d]".into(),
                "a stored Table View layout with an empty column, as a new layout has, since \
                 no field object is left"
                    .into(),
            ));
            member.set_child(view);
        }
        None => {}
    }
    Ok(())
}

/// Whether an entry's value `value` names `object`: the object's key
/// string as the named objects `08` hold it (`exact`), or the nine-byte
/// value of the object index and the tab order (a `u16` length, then the
/// key string).
fn names(value: &[u8], object: u64, exact: bool) -> bool {
    let key = key_string(object);
    if exact {
        return value == key;
    }
    indexed_object(value) == Some(object)
}

/// The object an object index or tab order entry's value names: a `u16`
/// length of the object's key string, the key string (length byte, serial
/// key) zero-padded to three bytes, then two `u16`s (layout-model.md 10.5).
pub(crate) fn indexed_object(value: &[u8]) -> Option<u64> {
    let [0, len, rest @ ..] = value else {
        return None;
    };
    if value.len() != 9 || !(1..=3).contains(len) {
        return None;
    }
    let (&n, key) = rest[..usize::from(*len)].split_first()?;
    (usize::from(n) == key.len())
        .then(|| serial::decode(key))
        .flatten()
}

/// Remove `objects` from the field reference index `01, <occurrence>,
/// <field>, <object>`, a field's node left without objects, and an
/// occurrence's left without fields unless it is the layout's own
/// (`own`), as the files FileMaker saved hold them (layout-model.md 10.8).
fn unreference(
    member: &mut OwnedNode,
    objects: &[u64],
    own: u64,
    changes: &mut Vec<(String, String)>,
) {
    let Some(index) = member.child(&[FIELD_INDEX]) else {
        return;
    };
    let mut index = index.clone();
    let mut removed = 0;
    // Only nodes this deletion empties are dropped; others stay as stored.
    let empty = |node: &OwnedNode| node.children.is_empty() && node.value.is_none();
    index.children.retain_mut(|occurrence| {
        let mut emptied = false;
        occurrence.children.retain_mut(|field| {
            let before = field.children.len();
            field.children.retain(|object| {
                serial::decode(&object.segment).is_none_or(|key| !objects.contains(&key))
            });
            removed += before - field.children.len();
            let drop = field.children.len() < before && empty(field);
            emptied |= drop;
            !drop
        });
        !(emptied && empty(occurrence) && serial::decode(&occurrence.segment) != Some(own))
    });
    if removed > 0 {
        changes.push((
            "[04, 05, L, 01]".into(),
            format!("{removed} field references removed"),
        ));
        member.set_child(index);
    }
}

/// Remove `objects` from their parts' lists `03, <part>, 07`, each list's
/// count lowered and every other byte kept. A list that does not read is
/// refused: every part FileMaker saved has a readable one.
fn unlist(
    member: &mut OwnedNode,
    objects: &[u64],
    changes: &mut Vec<(String, String)>,
) -> Result<(), Refusal> {
    let Some(parts) = member.child(&[PARTS]) else {
        return Ok(());
    };
    let mut parts = parts.clone();
    let mut changed = false;
    for part in &mut parts.children {
        let Some(list) = part.child(&[PART_OBJECTS]) else {
            continue;
        };
        let (value, removed) = list
            .value
            .as_deref()
            .filter(|_| list.children.is_empty())
            .and_then(|value| part_list_without(value, objects))
            .ok_or_else(|| {
                Refusal::Unsupported(format!(
                    "OpenMaker cannot read the list of objects of part {}.",
                    hex(&part.segment)
                ))
            })?;
        if removed == 0 {
            continue;
        }
        changes.push((
            format!("[04, 05, L, 03, {}, 07]", hex(&part.segment)),
            format!("{removed} entries removed"),
        ));
        part.set_child(OwnedNode::leaf(vec![PART_OBJECTS], value));
        changed = true;
    }
    if changed {
        member.set_child(parts);
    }
    Ok(())
}

/// A part's list `value` without its entries naming `objects`, and how many
/// it had; `None` when it does not read ([`part_list`]).
fn part_list_without(value: &[u8], objects: &[u64]) -> Option<(Vec<u8>, usize)> {
    let keys = part_list(value)?;
    let removed = keys.iter().filter(|k| objects.contains(k)).count();
    let count = u32::try_from(keys.len() - removed).ok()?;
    let mut out = count.to_be_bytes().to_vec();
    let mut rest = &value[4..];
    for key in keys {
        let entry = &rest[..1 + usize::from(rest[0])];
        if !objects.contains(&key) {
            out.extend_from_slice(entry);
        }
        rest = &rest[entry.len()..];
    }
    Some((out, removed))
}

/// The object index `0a` renumbered for a deletion (layout-model.md 10.5,
/// 10.8): each entry is keyed by its object's place in the object tree,
/// so the entries after a deleted object move up.
struct Renumbering<'a> {
    deleted: &'a [u64],
    /// The stored tree in pre-order.
    order: &'a [u64],
    /// Each object's place in the stored tree, and in the tree without the
    /// deleted objects.
    before: HashMap<u64, u16>,
    after: HashMap<u64, u16>,
    /// The tree's containers: a portal's entry holds its contents' entries
    /// and has no value, so it is read as the container at its place.
    containers: HashSet<u64>,
}

impl Renumbering<'_> {
    /// Remove the deleted objects' entries from `node` (the index, or an
    /// entry nested `depth` deep), and key every other entry at its
    /// object's new place, counting those renumbered. Each entry must be at
    /// its object's place in the stored tree, as every entry FileMaker saved
    /// is (layout-model.md 10.5); an index that is not is refused rather
    /// than guessed at.
    fn apply(
        &self,
        node: &mut OwnedNode,
        depth: usize,
        renumbered: &mut usize,
    ) -> Result<(), Refusal> {
        let refused = || {
            Err(Refusal::Unsupported(
                "The layout's object index does not follow its object tree; OpenMaker does not \
                 delete objects on it."
                    .into(),
            ))
        };
        if depth > MAX_INDEX_DEPTH {
            return refused();
        }
        let mut kept = Vec::with_capacity(node.children.len());
        for mut entry in std::mem::take(&mut node.children) {
            let [top_hi, top_lo, left_hi, left_lo, hi, lo] = entry.segment[..] else {
                return refused();
            };
            let place = u16::from_be_bytes([hi, lo]);
            let object = match entry.value.as_deref() {
                Some(value) => indexed_object(value),
                None => self
                    .order
                    .get(usize::from(place))
                    .copied()
                    .filter(|o| self.containers.contains(o)),
            };
            let Some(object) = object else {
                return refused();
            };
            if self.before.get(&object) != Some(&place) {
                return refused();
            }
            if self.deleted.contains(&object) {
                continue;
            }
            let Some(new) = self.after.get(&object) else {
                return refused();
            };
            let [hi, lo] = new.to_be_bytes();
            let segment = [top_hi, top_lo, left_hi, left_lo, hi, lo];
            if entry.segment != segment {
                entry.segment = segment.to_vec();
                *renumbered += 1;
            }
            self.apply(&mut entry, depth + 1, renumbered)?;
            kept.push(entry);
        }
        // Places keep their order, so the keys stay sorted and distinct.
        if kept.windows(2).any(|w| w[0].segment >= w[1].segment) {
            return refused();
        }
        node.children = kept;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_layout::encode::{bytes, double, field_object, text_object, uint};

    fn frame(top: f64, left: f64, bottom: f64, right: f64) -> Rect {
        Rect {
            top,
            left,
            bottom,
            right,
            other: Vec::new(),
        }
    }

    /// A Form layout 480 points wide, a 200-point body listing an edit box
    /// (object 1) and a text (object 2), the edit box in the object index at
    /// (20, 110), and an unknown key `77` in the member.
    fn member() -> OwnedNode {
        let mut def = Vec::new();
        uint(2, 1_065_090, &mut def);
        double(8, 480.0, &mut def);
        let mut body = Vec::new();
        uint(1, 4, &mut body);
        double(4, 0.0, &mut body);
        double(5, 200.0, &mut body);
        let mut store = Vec::new();
        uint(1, 1, &mut store);
        uint(1, 2, &mut store);
        bytes(
            2,
            &field_object(1, &frame(20.0, 110.0, 42.0, 230.0), 1_065_090, 2),
            &mut store,
        );
        bytes(
            2,
            &text_object(2, &frame(20.0, 20.0, 40.0, 100.0)),
            &mut store,
        );
        OwnedNode::list(
            vec![1],
            vec![
                OwnedNode::leaf(vec![2], def),
                OwnedNode::list(
                    vec![3],
                    vec![OwnedNode::list(
                        vec![1],
                        vec![
                            OwnedNode::leaf(vec![2], body),
                            OwnedNode::leaf(vec![7], vec![0, 0, 0, 2, 2, 1, 1, 2, 1, 2]),
                        ],
                    )],
                ),
                OwnedNode::leaf(vec![7], store),
                OwnedNode::list(
                    vec![0x0a],
                    vec![OwnedNode::leaf(
                        vec![0, 20, 0, 110, 0, 1],
                        vec![0, 2, 1, 1, 0, 0, 0, 0, 1],
                    )],
                ),
                OwnedNode::leaf(vec![0x77], vec![9]),
            ],
        )
    }

    /// A field shown as a pop-up menu takes display type 2, the value list
    /// and style type 63 in place, its flags, bounds and other bytes as
    /// stored; back as an edit box the list stays. A calendar on a field not a date, a list control without a
    /// list, a missing list, another field than checked, a text and an
    /// unknown control are refused.
    #[test]
    fn a_field_takes_its_control_in_place() {
        let member = member();
        let facts = ControlFacts {
            field: (1_065_090, 2),
            date: false,
            container: false,
            value_list_exists: true,
        };
        let shown = |m: &OwnedNode| {
            let (_, store) = stored_objects(m).unwrap();
            let o = store.objects.iter().find(|o| o.id == 1).unwrap().clone();
            let f = o.field_obj.clone().unwrap();
            let style = o.style.as_ref().unwrap();
            (
                f.display_type,
                f.value_list_key,
                f.flags,
                style.object_type_id,
                o.bounds,
            )
        };
        let popup = plan_control(&member, 1, (2, Some(3)), &facts)
            .unwrap()
            .member;
        let (display, list, flags, style, bounds) = shown(&popup);
        assert_eq!((display, list, style), (2, 3, 63));
        assert_eq!((flags, bounds), (shown(&member).2, shown(&member).4));
        assert!(already_control(&popup, 1, 2, Some(3)) && !already_control(&member, 1, 2, None));
        let edit = plan_control(&popup, 1, (0, None), &facts).unwrap().member;
        let (display, list, flags, style, _) = shown(&edit);
        assert_eq!((display, list, flags, style), (0, 3, shown(&member).2, 61));
        let missing = ControlFacts {
            value_list_exists: false,
            ..facts
        };
        let other = ControlFacts {
            field: (1_065_090, 9),
            ..facts
        };
        for refused in [
            plan_control(&member, 1, (6, None), &facts),
            plan_control(&member, 1, (3, None), &facts),
            plan_control(&member, 1, (2, Some(9)), &missing),
            // The list the edit box kept is gone from the file.
            plan_control(&edit, 1, (2, None), &missing),
            plan_control(&member, 1, (2, Some(3)), &other),
            plan_control(&member, 2, (2, Some(3)), &facts),
            plan_control(&member, 1, (5, Some(3)), &facts),
        ] {
            assert!(refused.is_err());
        }
    }

    #[test]
    fn a_moved_object_changes_its_bounds_and_index_entry_only() {
        let member = member();
        let moved = frame(60.0, 140.0, 82.0, 300.0);
        let planned = plan(
            &member,
            &[ObjectFrame {
                object: 1,
                frame: moved.clone(),
            }],
        )
        .unwrap();
        let (_, store) = stored_objects(&planned.member).unwrap();
        assert_eq!(store.objects[0].bounds.as_ref(), Some(&moved));
        assert_eq!(
            store.objects[1].bounds.as_ref(),
            Some(&frame(20.0, 20.0, 40.0, 100.0))
        );
        let index = planned.member.child(&[0x0a]).unwrap();
        assert_eq!(index.children.len(), 1);
        assert_eq!(index.children[0].segment, [0, 60, 0, 140, 0, 1]);
        assert_eq!(
            index.children[0].value.as_deref(),
            Some(&[0, 2, 1, 1, 0, 0, 0, 0, 1][..])
        );
        // Everything else of the member is as stored.
        for key in [[2u8], [3], [0x77]] {
            assert_eq!(planned.member.child(&key), member.child(&key));
        }
        assert!(already_framed(
            &planned.member,
            &[ObjectFrame {
                object: 1,
                frame: moved
            }]
        ));
        // A text has no index entry: only its bounds change.
        let label = plan(
            &member,
            &[ObjectFrame {
                object: 2,
                frame: frame(100.0, 20.0, 130.0, 100.0),
            }],
        )
        .unwrap();
        assert_eq!(label.member.child(&[0x0a]), member.child(&[0x0a]));
    }

    /// The nine-byte value of object `key`'s object index and tab order
    /// entries.
    fn indexed(key: u8) -> Vec<u8> {
        vec![0, 2, 1, key, 0, 0, 0, 0, 1]
    }

    /// A layout of occurrence 1065090 listing a text (object 1), an edit box
    /// of a related occurrence's field (object 2: indexed at place 1, in the
    /// tab order, named) and an edit box of its own occurrence's (object 3:
    /// indexed at place 2, in the tab order).
    fn deletable_member() -> OwnedNode {
        let own = 1_065_090;
        let related = 1_065_091;
        let mut def = Vec::new();
        uint(2, own, &mut def);
        double(8, 480.0, &mut def);
        let mut body = Vec::new();
        uint(1, 4, &mut body);
        double(4, 0.0, &mut body);
        double(5, 200.0, &mut body);
        let mut store = Vec::new();
        for key in 1..=3 {
            uint(1, key, &mut store);
        }
        bytes(
            2,
            &text_object(1, &frame(20.0, 20.0, 40.0, 100.0)),
            &mut store,
        );
        bytes(
            2,
            &field_object(2, &frame(20.0, 110.0, 42.0, 230.0), related, 5),
            &mut store,
        );
        bytes(
            2,
            &field_object(3, &frame(60.0, 110.0, 82.0, 230.0), own, 2),
            &mut store,
        );
        let occurrence = |id: u64, field: u8, object: u8| {
            OwnedNode::list(
                serial::encode(id),
                vec![OwnedNode::list(
                    vec![field],
                    vec![OwnedNode::leaf(vec![object], Vec::new())],
                )],
            )
        };
        let record =
            |key: u8| OwnedNode::list(vec![key], vec![OwnedNode::leaf(vec![0x10], Vec::new())]);
        OwnedNode::list(
            vec![1],
            vec![
                OwnedNode::list(
                    vec![1],
                    vec![occurrence(own, 2, 3), occurrence(related, 5, 2)],
                ),
                OwnedNode::leaf(vec![2], def),
                OwnedNode::list(
                    vec![3],
                    vec![OwnedNode::list(
                        vec![1],
                        vec![
                            OwnedNode::leaf(vec![2], body),
                            OwnedNode::leaf(vec![7], vec![0, 0, 0, 3, 2, 1, 1, 2, 1, 2, 2, 1, 3]),
                        ],
                    )],
                ),
                OwnedNode::list(
                    vec![5],
                    vec![
                        OwnedNode::leaf(vec![0], vec![1, 3]),
                        record(1),
                        record(2),
                        record(3),
                    ],
                ),
                OwnedNode::leaf(vec![7], store),
                OwnedNode::list(vec![8], vec![OwnedNode::leaf(vec![0x40, 0x41], vec![1, 2])]),
                OwnedNode::list(
                    vec![0x0a],
                    vec![
                        OwnedNode::leaf(vec![0, 20, 0, 110, 0, 1], indexed(2)),
                        OwnedNode::leaf(vec![0, 60, 0, 110, 0, 2], indexed(3)),
                    ],
                ),
                OwnedNode::list(
                    vec![0x15],
                    vec![
                        OwnedNode::leaf(vec![0, 1], indexed(2)),
                        OwnedNode::leaf(vec![0, 4], indexed(3)),
                    ],
                ),
                OwnedNode::leaf(vec![0x77], vec![9]),
            ],
        )
    }

    /// Deleting the last field object leaves a stored table view Pro can
    /// draw (F88, #814): its column taken out and the empty column renewed,
    /// full width; a layout with none gets a new layout's.
    #[test]
    fn deleting_the_last_field_leaves_a_table_view_with_an_empty_column() {
        use crate::layout_placement::{planned_table_view, Placement};
        let own = 0xd0_0002;
        // No stored table view: one appears only once no field is left.
        let member = deletable_member();
        let one = plan_deletion(&member, &[2]).unwrap();
        assert!(one.member.child(&[TABLE_VIEW]).is_none());
        let both = plan_deletion(&member, &[2, 3]).unwrap();
        let view = both.member.child(&[TABLE_VIEW]).unwrap();
        assert_eq!(view, &crate::layout_catalog::table_view(own).unwrap());

        // A stored table view with object 3's field as its column, as a
        // placement writes it (#815).
        let mut placed = member.clone();
        placed.set_child(crate::layout_catalog::table_view(own).unwrap());
        let placement = Placement {
            occurrence: own,
            field: 2,
            frame: frame(60.0, 110.0, 82.0, 230.0),
            label: None,
        };
        let view = planned_table_view(&placed, &placement).unwrap().unwrap();
        placed.set_child(view);
        // Object 2 shows another field: deleting 3 takes its column out.
        let planned = plan_deletion(&placed, &[3]).unwrap();
        let view = planned.member.child(&[TABLE_VIEW]).unwrap();
        let (_, columns) = stored_objects(view).unwrap();
        assert_eq!(columns.root, [5]);
        let empty = &columns.objects[0];
        assert_ne!(empty.flags & om_layout::encode::EMPTY_COLUMN_FLAG, 0);
        let bounds = empty.bounds.clone().unwrap();
        assert_eq!((bounds.left, bounds.right), (0.0, 120.0));
        let references = view.child(&[1]).unwrap();
        assert!(references.at(&[&serial::encode(1_065_090), &[2]]).is_none());
        assert!(references.at(&[&[0], &[0], &[5]]).is_some());
        assert_eq!(view.child(&[0x0a]).unwrap().children.len(), 1);
        let tab: Vec<&[u8]> = view
            .child(&[0x15])
            .unwrap()
            .children
            .iter()
            .map(|t| &t.segment[..])
            .collect();
        assert_eq!(tab, [&[0, 1][..]]);
        assert_eq!(
            view.at(&[&[5], &[0]]).unwrap().value.as_deref(),
            Some(&[1, 5][..])
        );
        // A table view with no column (an empty object store, as Pro writes
        // one on a theme change) gets one when the last field goes, in the
        // body's row; an unrelated empty field node is kept.
        let mut columnless = member.clone();
        let mut empty_view = crate::layout_catalog::table_view(own).unwrap();
        empty_view.set_child(stored_value(&[OBJECTS], &[]));
        empty_view.put(
            &[&[3], &[2], &[7]],
            OwnedNode::leaf(vec![7], vec![0, 0, 0, 0]),
        );
        empty_view.set_child(OwnedNode::list(vec![0x0a], Vec::new()));
        empty_view.set_child(OwnedNode::list(vec![0x15], Vec::new()));
        empty_view.remove(&[&[1], &[0]]);
        empty_view.put(
            &[&[1], &serial::encode(1_065_090), &[9]],
            OwnedNode::list(vec![9], Vec::new()),
        );
        columnless.set_child(empty_view);
        let repaired = plan_deletion(&columnless, &[2, 3]).unwrap();
        let view = repaired.member.child(&[TABLE_VIEW]).unwrap();
        let (_, columns) = stored_objects(view).unwrap();
        assert_eq!(columns.root, [3]);
        let bounds = columns.objects[0].bounds.clone().unwrap();
        assert_eq!(
            (bounds.top, bounds.bottom, bounds.left, bounds.right),
            (24.0, 45.0, 0.0, 120.0)
        );
        assert!(view.at(&[&[1], &serial::encode(1_065_090), &[9]]).is_some());

        // While another object shows the field, its column stays.
        let mut twice = placed.clone();
        let (stored, _) = stored_objects(&twice).unwrap();
        let again = om_layout::encode::append_to_store(
            &stored,
            &[4],
            &[field_object(
                4,
                &frame(100.0, 110.0, 122.0, 230.0),
                1_065_090,
                2,
            )],
        );
        twice.set_child(stored_value(&[OBJECTS], &again));
        let kept = plan_deletion(&twice, &[3]).unwrap();
        assert_eq!(kept.member.child(&[TABLE_VIEW]), twice.child(&[TABLE_VIEW]));
    }

    #[test]
    fn a_deleted_object_leaves_no_record_and_the_index_follows_the_tree() {
        let member = deletable_member();
        let planned = plan_deletion(&member, &[2]).unwrap();
        let after = &planned.member;
        let (_, store) = stored_objects(after).unwrap();
        assert_eq!(store.root, [1, 3]);
        assert_eq!(
            store.objects.iter().map(|o| o.id).collect::<Vec<_>>(),
            [1, 3]
        );
        // Its attribute record goes; the last key issued stays.
        let records = after.child(&[5]).unwrap();
        let keys: Vec<&[u8]> = records.children.iter().map(|r| &r.segment[..]).collect();
        assert_eq!(keys, [&[0][..], &[1], &[3]]);
        assert_eq!(after.at(&[&[5], &[0]]), member.at(&[&[5], &[0]]));
        // The related occurrence it alone showed goes from the field
        // references; the layout's own stays.
        let references = after.child(&[1]).unwrap();
        assert_eq!(references.children.len(), 1);
        assert_eq!(
            references.children[0],
            member.child(&[1]).unwrap().children[0]
        );
        // Its part list entry, named entry and tab order entry go; the tab
        // order keeps its numbers.
        assert_eq!(
            after.at(&[&[3], &[1], &[7]]).unwrap().value.as_deref(),
            Some(&[0, 0, 0, 2, 2, 1, 1, 2, 1, 3][..])
        );
        assert!(after.child(&[8]).unwrap().children.is_empty());
        let tab = after.child(&[0x15]).unwrap();
        assert_eq!(tab.children.len(), 1);
        assert_eq!(tab.children[0].segment, [0, 4]);
        // Its index entry goes, and object 3's moves up to its new place.
        let index = after.child(&[0x0a]).unwrap();
        assert_eq!(index.children.len(), 1);
        assert_eq!(index.children[0].segment, [0, 60, 0, 110, 0, 1]);
        assert_eq!(index.children[0].value.as_deref(), Some(&indexed(3)[..]));
        for key in [[2u8], [0x77]] {
            assert_eq!(after.child(&key), member.child(&key));
        }
        assert!(already_deleted(after, &[2]));
        assert!(!already_deleted(&member, &[2]));
        // Unknown and twice-named objects are refused.
        assert!(matches!(
            plan_deletion(&member, &[9]),
            Err(Refusal::Invalid(_))
        ));
        assert!(matches!(
            plan_deletion(&member, &[3, 3]),
            Err(Refusal::Invalid(_))
        ));
        // An index entry off its object's place is refused, not guessed at.
        let mut off = member.clone();
        let mut index = off.child(&[0x0a]).unwrap().clone();
        index.children[1].segment = vec![0, 60, 0, 110, 0, 7];
        off.set_child(index);
        assert!(matches!(
            plan_deletion(&off, &[2]),
            Err(Refusal::Unsupported(_))
        ));
    }

    /// A field's label, a popover's button and other objects another one
    /// names by key are not deleted, unless the one naming it goes too.
    #[test]
    fn objects_other_objects_name_are_not_deleted() {
        let object = |id, object_type, label_key| LayoutObject {
            id,
            object_type,
            label_key,
            ..Default::default()
        };
        let store = om_layout::ObjectStore {
            root: vec![1, 2, 3],
            objects: vec![object(1, 1, 2), object(2, 2, 0), object(3, 14, 0)],
            ..Default::default()
        };
        assert!(deletable(&store, 1).is_ok());
        assert!(matches!(deletable(&store, 2), Err(Refusal::Unsupported(_))));
        assert!(deletable_with(&store, 2, &[1, 2]).is_ok());
        assert!(matches!(deletable(&store, 3), Err(Refusal::Unsupported(_))));
    }

    #[test]
    fn objects_showing_library_images_are_not_deleted() {
        use om_layout::object::ButtonObject;
        use om_layout::style::{ComponentStyle, ObjectStyle, Property, StyleSet};
        use om_layout::wire::{RawField, RawValue};
        let button = |library_key| LayoutObject {
            button: Some(ButtonObject {
                library_key,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(uses_library(&button(3)));
        assert!(!uses_library(&button(0)));
        // An image in the object's own style: `Image.libraryKey` (field 3).
        let styled = |image: Vec<u8>| LayoutObject {
            style: Some(ObjectStyle {
                local_styles: Some(StyleSet {
                    components: vec![ComponentStyle {
                        properties: vec![Property {
                            id: 1,
                            other: vec![RawField {
                                number: 18,
                                value: RawValue::Bytes(image),
                            }],
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(uses_library(&styled(vec![0x08, 0x02, 0x18, 0x05])));
        assert!(!uses_library(&styled(vec![0x08, 0x00])));
        assert!(uses_library(&styled(vec![0x18])));
    }

    #[test]
    fn frames_outside_the_part_or_width_and_unknown_objects_are_refused() {
        let member = member();
        let refused =
            |object: u64, f: Rect| plan(&member, &[ObjectFrame { object, frame: f }]).unwrap_err();
        // Below the body, past the width, between points, unknown, twice.
        assert!(matches!(
            refused(1, frame(190.0, 10.0, 212.0, 100.0)),
            Refusal::Invalid(_)
        ));
        assert!(matches!(
            refused(1, frame(10.0, 400.0, 32.0, 500.0)),
            Refusal::Invalid(_)
        ));
        assert!(matches!(
            refused(1, frame(10.5, 10.0, 32.0, 100.0)),
            Refusal::Invalid(_)
        ));
        assert!(matches!(
            refused(9, frame(10.0, 10.0, 32.0, 100.0)),
            Refusal::Invalid(_)
        ));
        let twice = ObjectFrame {
            object: 1,
            frame: frame(10.0, 10.0, 32.0, 100.0),
        };
        assert!(plan(&member, &[twice.clone(), twice]).is_err());
    }
}
