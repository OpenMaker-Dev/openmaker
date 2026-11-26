//! Writing new layout objects: the part of the protobuf wire format (proto2,
//! `docs/re/protobuf/LayoutObject.proto`) needed to encode the objects
//! OpenMaker places, laid out as FileMaker Pro's serializer writes them
//! (fields in number order, repeated scalars unpacked, every set scalar
//! written even when zero), and to add them to a stored
//! `LayoutObjectStore` without rewriting what it holds.
//!
//! The defaults are those of the objects FileMaker Pro made in the shipped
//! templates (layout-model.md 5.3, 5.4, 5.7): anchored left and top, a
//! local style holding only the rotation, styled by the object type the
//! theme styles, so a new object takes the layout's theme.

use crate::object::Rect;
use crate::wire::{Fields, Value};

/// `LayoutObject.type` of a field, a text object and a button
/// (layout-model.md 5.3).
pub const FIELD: u32 = 1;
pub const TEXT: u32 = 2;
pub const PORTAL: u32 = 9;
pub const BUTTON: u32 = 10;

/// Style object types (`ObjectStyle.objectTypeId`, layout-model.md 6.2):
/// an edit box, and a text box.
pub const EDIT_BOX_STYLE: u64 = 61;
pub const TEXT_BOX_STYLE: u64 = 70;
pub const BUTTON_STYLE: u64 = 80;
pub const PORTAL_STYLE: u64 = 83;

/// `PortalLayoutObject.portalFlags` of a portal FileMaker Pro 26.0.3 drew
/// with Portal Setup's defaults: `0x10` alone (layout-model.md 5.3.2; the
/// shipped templates' portals add `0x1` and `0x100`).
pub const PORTAL_FLAGS: u64 = 0x10;

/// `FieldLayoutObject.flags` of a field FileMaker Pro 26.0.3 put in a new
/// portal with Add Fields to Portal (`0x20`, as on its table view columns;
/// layout-model.md 5.3.2, kept as observed).
pub const PORTAL_FIELD_FLAGS: u64 = 0x20;

/// `ButtonLayoutObject.textFlags` and `iconSize` of the templates' text
/// buttons with a plain label that perform a script (`0x2`, 16; meanings
/// not decoded; layout-model.md 5.3.3).
pub const BUTTON_TEXT_FLAGS: u64 = 2;
pub const BUTTON_ICON_SIZE: u64 = 16;

/// `LayoutObject.flags` of an object anchored left and top, the default
/// (layout-model.md 5.7), with no other bit.
pub const ANCHOR_LEFT_TOP: u64 = 0x3000_0000;

/// `FieldLayoutObject.flags` of a new edit box: the bits every enterable
/// edit box in the shipped templates has (`0x20`, `0x40`, `0x80`; their
/// meanings are not decoded), without the placeholder (`0x20000`), scroll
/// bar (`0x100`) or other bits some of them add.
pub const EDIT_BOX_FLAGS: u64 = 0xe0;

/// `TextLayoutObject.flags` most of the templates' text objects have
/// (meaning not decoded).
pub const TEXT_FLAGS: u64 = 2;

/// Style property 613, `-fm-rotation`: FileMaker Pro moves it into the
/// object on load and writes it back in the local style (layout-model.md 3).
const ROTATION: u64 = 613;

/// The `self` component and the `normal` state (layout-model.md 6.3, 6.4).
const SELF: u64 = 1;
const NORMAL: u64 = 1;

/// Append `v` as a base-128 varint.
pub fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn tag(number: u32, wire_type: u8, out: &mut Vec<u8>) {
    varint(u64::from(number) << 3 | u64::from(wire_type), out);
}

/// A varint field.
pub fn uint(number: u32, v: u64, out: &mut Vec<u8>) {
    tag(number, 0, out);
    varint(v, out);
}

/// A double field (fixed64).
pub fn double(number: u32, v: f64, out: &mut Vec<u8>) {
    tag(number, 1, out);
    out.extend_from_slice(&v.to_le_bytes());
}

/// A length-delimited field: bytes, a string or an embedded message.
pub fn bytes(number: u32, b: &[u8], out: &mut Vec<u8>) {
    tag(number, 2, out);
    varint(b.len() as u64, out);
    out.extend_from_slice(b);
}

/// `FMLayoutPB.Rect`.
pub fn rect(bounds: &Rect) -> Vec<u8> {
    let mut out = Vec::new();
    double(1, bounds.top, &mut out);
    double(2, bounds.left, &mut out);
    double(3, bounds.bottom, &mut out);
    double(4, bounds.right, &mut out);
    out
}

/// The `ObjectStyle` FileMaker Pro writes for an object without local
/// styling: the rotation 0 in the normal state of `self`, and the style
/// object type, so the theme's style for that type applies.
pub fn default_style(object_type: u64) -> Vec<u8> {
    let mut property = Vec::new();
    uint(1, ROTATION, &mut property);
    uint(3, 0, &mut property);
    let mut properties = Vec::new();
    bytes(1, &property, &mut properties);
    let mut state = Vec::new();
    uint(1, SELF, &mut state);
    uint(2, NORMAL, &mut state);
    bytes(3, &properties, &mut state);
    let mut local = Vec::new();
    bytes(1, &state, &mut local);
    let mut style = Vec::new();
    bytes(1, &local, &mut style);
    uint(3, object_type, &mut style);
    style
}

/// A field object `key` at `bounds`: an edit box showing the first
/// repetition of field `field_key` of occurrence `table_key`, both as a
/// field spec stores them (the serial values of their keys, layout-model.md
/// 4.1), with one repetition, no value list and the templates' keyboard
/// and scroll bar settings.
pub fn field_object(key: u64, bounds: &Rect, table_key: u64, field_key: u64) -> Vec<u8> {
    edit_box(
        key,
        bounds,
        ANCHOR_LEFT_TOP,
        EDIT_BOX_FLAGS,
        (table_key, field_key),
    )
}

/// A field object `key` in a portal, at `bounds` relative to the portal:
/// an edit box showing the first repetition of field `field_key` of the
/// portal's occurrence `table_key`, with the flags FileMaker Pro gives a
/// field it adds to a new portal.
pub fn portal_field_object(key: u64, bounds: &Rect, table_key: u64, field_key: u64) -> Vec<u8> {
    edit_box(
        key,
        bounds,
        ANCHOR_LEFT_TOP,
        PORTAL_FIELD_FLAGS,
        (table_key, field_key),
    )
}

/// A portal `key` at `bounds` showing `rows` rows of the occurrence whose
/// field-spec key is `table_key`, with `children` its contents, as
/// FileMaker Pro 26.0.3 stores a portal drawn with Portal Setup's defaults
/// (layout-model.md 5.3.2): no table context, the default flags, the first
/// row first, the row count as a double and scroll bar style 1.
pub fn portal_object(
    key: u64,
    bounds: &Rect,
    children: &[u64],
    table_key: u64,
    rows: u32,
) -> Vec<u8> {
    let mut portal = Vec::new();
    // tableContextKey, tableAliasKey, portalFlags, initialRow, numRows,
    // scrollbarStyle.
    uint(1, 0, &mut portal);
    uint(2, table_key, &mut portal);
    uint(3, PORTAL_FLAGS, &mut portal);
    uint(5, 1, &mut portal);
    double(6, f64::from(rows), &mut portal);
    uint(7, 1, &mut portal);
    let mut out = Vec::new();
    uint(1, u64::from(PORTAL), &mut out);
    uint(2, key, &mut out);
    uint(3, ANCHOR_LEFT_TOP, &mut out);
    bytes(4, &rect(bounds), &mut out);
    bytes(5, &default_style(PORTAL_STYLE), &mut out);
    for &child in children {
        uint(6, child, &mut out);
    }
    bytes(15, &portal, &mut out);
    out
}

/// `LayoutObject.flags` bits of a table view's objects (layout-model.md
/// 5.2.1): `kTableView`, and `kEmptyColumn` on its empty column.
pub const TABLE_VIEW_FLAG: u64 = 0x80;
pub const EMPTY_COLUMN_FLAG: u64 = 0x0400_0000;

/// A stored table view's empty column `key` at `bounds` (layout-model.md
/// 10.14): an edit box showing no field (occurrence and field 0), anchored
/// left and top, with `kEmptyColumn` and `kTableView` and no field flags.
pub fn empty_table_column(key: u64, bounds: &Rect) -> Vec<u8> {
    let flags = ANCHOR_LEFT_TOP | EMPTY_COLUMN_FLAG | TABLE_VIEW_FLAG;
    edit_box(key, bounds, flags, 0, (0, 0))
}

/// A stored table view's column `key` at `bounds` showing the first
/// repetition of field `field_key` of occurrence `table_key` (layout-model.md
/// 10.14): an edit box anchored left and top with `kTableView`, with the
/// field flags of Pro's own columns.
pub fn table_column(key: u64, bounds: &Rect, table_key: u64, field_key: u64) -> Vec<u8> {
    let flags = ANCHOR_LEFT_TOP | TABLE_VIEW_FLAG;
    edit_box_with(
        key,
        bounds,
        flags,
        TABLE_COLUMN_FIELD_FLAGS,
        (table_key, field_key),
        TABLE_COLUMN_REPETITIONS,
    )
}

/// `FieldLayoutObject.numOfReps` of a field's column in a stored table
/// view, as FileMaker Pro 26.0.3 writes it: -1, an `int32` in ten varint
/// bytes (layout-model.md 10.14; meaning not decoded).
pub const TABLE_COLUMN_REPETITIONS: u64 = u64::MAX;

/// `FieldLayoutObject.flags` of a field's column in a stored table view, as
/// FileMaker Pro 26.0.3 wrote it when its Field tool placed a field (`0x20`;
/// layout-model.md 10.14, kept as observed: the bits are not decoded).
pub const TABLE_COLUMN_FIELD_FLAGS: u64 = 0x20;

/// An edit box `key` at `bounds` with object `flags` and field `field_flags`
/// showing the first repetition of `(occurrence, field)`, as a field spec
/// stores them, with one repetition, no value list and the templates'
/// keyboard and scroll bar settings.
fn edit_box(key: u64, bounds: &Rect, flags: u64, field_flags: u64, shows: (u64, u64)) -> Vec<u8> {
    edit_box_with(key, bounds, flags, field_flags, shows, 1)
}

/// [`edit_box`] with `numOfReps` `repetitions`.
fn edit_box_with(
    key: u64,
    bounds: &Rect,
    flags: u64,
    field_flags: u64,
    shows: (u64, u64),
    repetitions: u64,
) -> Vec<u8> {
    let mut spec = Vec::new();
    uint(1, shows.0, &mut spec);
    uint(2, shows.1, &mut spec);
    uint(3, 1, &mut spec);
    let mut field = Vec::new();
    uint(1, field_flags, &mut field);
    bytes(2, &spec, &mut field);
    // numOfReps, inputMode, displayType (edit box), keyboardType,
    // scrollbarStyle.
    uint(3, repetitions, &mut field);
    uint(4, 0, &mut field);
    uint(5, 0, &mut field);
    uint(7, 1, &mut field);
    uint(8, 1, &mut field);
    let mut out = Vec::new();
    uint(1, u64::from(FIELD), &mut out);
    uint(2, key, &mut out);
    uint(3, flags, &mut out);
    bytes(4, &rect(bounds), &mut out);
    bytes(5, &default_style(EDIT_BOX_STYLE), &mut out);
    bytes(8, &field, &mut out);
    out
}

/// A button `key` at `bounds` that shows its label only (no icon, display
/// type 0) and runs one step: `opcode`, its `mStepValue` (`value`), its
/// flags and its 14 `mReserved` bytes, as `LayoutObject.proto`'s
/// `ScriptStep` stores them (script-steps.md 2). The label and the step's
/// parameters are stored beside the object store, in the layout's object
/// attributes (layout-model.md 5.3.3). Fields at their zero default are
/// left out.
pub fn button_object(
    key: u64,
    bounds: &Rect,
    opcode: u16,
    value: u16,
    flags: u32,
    reserved: &[u8; 14],
) -> Vec<u8> {
    let mut step = Vec::new();
    uint(1, u64::from(opcode), &mut step);
    if value != 0 {
        uint(2, u64::from(value), &mut step);
    }
    if flags != 0 {
        uint(3, u64::from(flags), &mut step);
    }
    bytes(4, reserved, &mut step);
    let mut button = Vec::new();
    uint(2, BUTTON_TEXT_FLAGS, &mut button);
    bytes(3, &step, &mut button);
    uint(5, BUTTON_ICON_SIZE, &mut button);
    let mut out = Vec::new();
    uint(1, u64::from(BUTTON), &mut out);
    uint(2, key, &mut out);
    uint(3, ANCHOR_LEFT_TOP, &mut out);
    bytes(4, &rect(bounds), &mut out);
    bytes(5, &default_style(BUTTON_STYLE), &mut out);
    bytes(16, &button, &mut out);
    out
}

/// A text object `key` at `bounds`. Its text is stored beside the object
/// store, in the layout's object attributes (layout-model.md 5.5).
pub fn text_object(key: u64, bounds: &Rect) -> Vec<u8> {
    let mut text = Vec::new();
    uint(1, TEXT_FLAGS, &mut text);
    let mut out = Vec::new();
    uint(1, u64::from(TEXT), &mut out);
    uint(2, key, &mut out);
    uint(3, ANCHOR_LEFT_TOP, &mut out);
    bytes(4, &rect(bounds), &mut out);
    bytes(5, &default_style(TEXT_BOX_STYLE), &mut out);
    bytes(7, &text, &mut out);
    out
}

/// A stored `LayoutObjectStore` (empty for none) with `objects` added on
/// top of the root stack: `roots` as `rootChildKey` (field 1) and each
/// object as an `objectDef` (field 2), appended to the encoding. A proto2
/// parser merges repeated fields across the whole message, so the store
/// decodes as the stored roots and objects in their order, then the new
/// ones; every stored byte, unknown fields included, stays as it is.
pub fn append_to_store(store: &[u8], roots: &[u64], objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = store.to_vec();
    for &root in roots {
        uint(1, root, &mut out);
    }
    for object in objects {
        bytes(2, object, &mut out);
    }
    out
}

/// The spans of a message's top-level fields: each field's number, value
/// and where its encoding starts and ends. `None` when the message does not
/// read.
fn spans<'a>(message: &'a [u8], name: &'static str) -> Option<Vec<(u32, Value<'a>, usize, usize)>> {
    let mut fields = Fields::new(message, name);
    let mut out = Vec::new();
    loop {
        let start = fields.offset();
        match fields.next() {
            None => return Some(out),
            Some(Ok(field)) => out.push((field.number, field.value, start, fields.offset())),
            Some(Err(_)) => return None,
        }
    }
}

/// A stored `FMLayoutPB.Rect` with its edges set to `bounds`: the four
/// doubles rewritten where they are stored, any missing one added, and
/// every other byte kept.
fn with_edges(stored: &[u8], bounds: &Rect) -> Option<Vec<u8>> {
    let edges = [bounds.top, bounds.left, bounds.bottom, bounds.right];
    let mut out = Vec::with_capacity(stored.len() + 36);
    let mut written = [false; 4];
    let mut at = 0;
    for (number, value, start, end) in spans(stored, "Rect")? {
        out.extend_from_slice(&stored[at..start]);
        match (number, value) {
            (1..=4, Value::Fixed64(_)) if !written[number as usize - 1] => {
                double(number, edges[number as usize - 1], &mut out);
                written[number as usize - 1] = true;
            }
            // A second copy of an edge, or an edge not stored as a double:
            // not a frame this can set.
            (1..=4, _) => return None,
            _ => out.extend_from_slice(&stored[start..end]),
        }
        at = end;
    }
    for (i, done) in written.iter().enumerate() {
        if !done {
            double(i as u32 + 1, edges[i], &mut out);
        }
    }
    Some(out)
}

/// A stored `LayoutObjectStore` with object `key`'s `bounds` (field 4 of
/// its `LayoutObject`) set to `bounds` (layout-model.md 5.1, 5.2): only that
/// field's bytes, and the lengths that hold them, change; every other
/// object, field and unknown byte stays as stored, in its place. `None`
/// when the store or the object does not read, when no object or more than
/// one has the key, or when the object holds no single bounds field.
pub fn with_object_bounds(store: &[u8], key: u64, bounds: &Rect) -> Option<Vec<u8>> {
    let fields = spans(store, "LayoutObjectStore")?;
    let mut found = None;
    for (index, (number, value, ..)) in fields.iter().enumerate() {
        let (2, Value::Bytes(object)) = (number, value) else {
            continue;
        };
        let object_fields = spans(object, "LayoutObject")?;
        let is_it = object_fields
            .iter()
            .any(|(n, v, ..)| *n == 2 && *v == Value::Varint(key));
        if is_it {
            if found.is_some() {
                return None;
            }
            found = Some((index, object_fields));
        }
    }
    let (index, object_fields) = found?;
    let (_, Value::Bytes(object), start, end) = fields[index] else {
        return None;
    };
    let mut bounds_fields = object_fields.iter().filter(|(n, ..)| *n == 4);
    let (_, Value::Bytes(stored), b_start, b_end) = *bounds_fields.next()? else {
        return None;
    };
    if bounds_fields.next().is_some() {
        return None;
    }
    let mut new_object = object[..b_start].to_vec();
    bytes(4, &with_edges(stored, bounds)?, &mut new_object);
    new_object.extend_from_slice(&object[b_end..]);
    let mut out = store[..start].to_vec();
    bytes(2, &new_object, &mut out);
    out.extend_from_slice(&store[end..]);
    Some(out)
}

/// A stored `LayoutDef` with its width `baseWidth` (field 8) set to
/// `width`: the one double replaced in its place, or added at the end when
/// the definition has none, every other field and byte kept as stored.
/// `None` when the definition does not read or holds more than one width,
/// or one not stored as a double.
pub fn with_base_width(def: &[u8], width: f64) -> Option<Vec<u8>> {
    with_double(def, "LayoutDef", 8, width)
}

/// `LayoutDef.customMargins`, Layout Setup's fixed page margins (a `Rect`;
/// layout-model.md 4.1, 4.2).
pub const CUSTOM_MARGINS_FIELD: u32 = 12;

/// A stored `LayoutDef` with its fixed page margins `customMargins` set to
/// `margins` (insets: top, left, bottom, right): the four doubles of the
/// one stored `Rect` rewritten where they are, or a `Rect` added at the end
/// when the definition has none, every other field and byte kept as stored.
/// `None` when the definition does not read, or holds the field more than
/// once or not as a message.
pub fn with_custom_margins(def: &[u8], margins: &Rect) -> Option<Vec<u8>> {
    let fields = spans(def, "LayoutDef")?;
    let mut found = fields.iter().filter(|(n, ..)| *n == CUSTOM_MARGINS_FIELD);
    let mut out = Vec::with_capacity(def.len() + 38);
    match (found.next(), found.next()) {
        (None, _) => {
            out.extend_from_slice(def);
            bytes(CUSTOM_MARGINS_FIELD, &rect(margins), &mut out);
        }
        (Some((_, Value::Bytes(stored), start, end)), None) => {
            out.extend_from_slice(&def[..*start]);
            bytes(
                CUSTOM_MARGINS_FIELD,
                &with_edges(stored, margins)?,
                &mut out,
            );
            out.extend_from_slice(&def[*end..]);
        }
        _ => return None,
    }
    Some(out)
}

/// `PartDef.offset` (field 4) and `PartDef.size` (field 5), doubles
/// (layout-model.md 4.3).
pub const PART_OFFSET_FIELD: u32 = 4;
pub const PART_SIZE_FIELD: u32 = 5;

/// A stored message `name` with its double field `number` set to `value`:
/// the one double replaced in its place, or added at the end when the
/// message has none, every other field and byte kept as stored. `None` when
/// the message does not read or holds the field more than once, or not as
/// a double.
pub fn with_double(message: &[u8], name: &'static str, number: u32, value: f64) -> Option<Vec<u8>> {
    let fields = spans(message, name)?;
    let mut found = fields.iter().filter(|(n, ..)| *n == number);
    let mut out = Vec::with_capacity(message.len() + 9);
    match (found.next(), found.next()) {
        (None, _) => {
            out.extend_from_slice(message);
            double(number, value, &mut out);
        }
        (Some((_, Value::Fixed64(_), start, end)), None) => {
            out.extend_from_slice(&message[..*start]);
            double(number, value, &mut out);
            out.extend_from_slice(&message[*end..]);
        }
        _ => return None,
    }
    Some(out)
}

/// What a field object's control style sets (layout-model.md 5.4): the
/// `FieldLayoutObject`'s `flags` (1), `displayType` (5) and `valueListKey`
/// (6), and its style's `objectTypeId` (3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldControl {
    pub flags: u64,
    pub display_type: u64,
    pub value_list_key: u64,
    pub style_type: u64,
}

/// A stored `LayoutObjectStore` with object `key`'s control set to
/// `control`. In its `fieldObj` (field 8) the flags, display type and value
/// list key are replaced in place, or added in field number order when
/// missing and not 0. In its style (field 5) the object type is replaced and the
/// theme's custom style names (2 and 4, which name styles of the old type)
/// are removed, so the object takes its new type's default from the file's
/// theme. Every other field and byte stays as stored. `None` when the store
/// or the object does not read, no object or more than one has the key, or
/// the object has no single field object or style.
pub fn with_field_control(store: &[u8], key: u64, control: &FieldControl) -> Option<Vec<u8>> {
    let fields = spans(store, "LayoutObjectStore")?;
    let mut found = None;
    for (index, (number, value, ..)) in fields.iter().enumerate() {
        let (2, Value::Bytes(object)) = (number, value) else {
            continue;
        };
        let object_fields = spans(object, "LayoutObject")?;
        if object_fields
            .iter()
            .any(|(n, v, ..)| *n == 2 && *v == Value::Varint(key))
        {
            if found.is_some() {
                return None;
            }
            found = Some((index, object_fields));
        }
    }
    let (index, object_fields) = found?;
    let (_, Value::Bytes(object), start, end) = fields[index] else {
        return None;
    };
    let single = |number: u32| {
        let mut matching = object_fields.iter().filter(|(n, ..)| *n == number);
        match (matching.next(), matching.next()) {
            (Some((_, Value::Bytes(_), ..)), None) => Some(()),
            _ => None,
        }
    };
    single(8)?;
    single(5)?;
    let mut new_object = Vec::with_capacity(object.len() + 16);
    for (number, value, f_start, f_end) in &object_fields {
        match (number, value) {
            (8, Value::Bytes(field)) => {
                let set = [
                    (1, control.flags),
                    (5, control.display_type),
                    (6, control.value_list_key),
                ];
                bytes(
                    8,
                    &with_varints(field, "FieldLayoutObject", &set)?,
                    &mut new_object,
                );
            }
            (5, Value::Bytes(style)) => {
                let mut out = Vec::with_capacity(style.len());
                let mut at = 0;
                for (n, value, s, e) in spans(style, "ObjectStyle")? {
                    out.extend_from_slice(&style[at..s]);
                    // A field of these numbers in another wire type is not
                    // understood: the edit is refused rather than lose it.
                    match (n, value) {
                        (2 | 4, Value::Bytes(_)) => {}
                        (3, Value::Varint(_)) => uint(3, control.style_type, &mut out),
                        (2..=4, _) => return None,
                        _ => out.extend_from_slice(&style[s..e]),
                    }
                    at = e;
                }
                out.extend_from_slice(&style[at..]);
                if !spans(style, "ObjectStyle")?.iter().any(|(n, ..)| *n == 3) {
                    uint(3, control.style_type, &mut out);
                }
                bytes(5, &out, &mut new_object);
            }
            _ => new_object.extend_from_slice(&object[*f_start..*f_end]),
        }
    }
    let mut out = store[..start].to_vec();
    bytes(2, &new_object, &mut out);
    out.extend_from_slice(&store[end..]);
    Some(out)
}

/// `message` with each varint field of `set` holding its value: replaced in
/// place (a second copy dropped), or, when missing and not 0, added in
/// field number order, before the first field with a higher number, as
/// FileMaker Pro writes it (layout-model.md 10.13). `None` when the message
/// does not read or one of those fields is not a varint.
fn with_varints(message: &[u8], name: &'static str, set: &[(u32, u64)]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(message.len() + 12);
    let fields = spans(message, name)?;
    // A stored field is rewritten where it stands; only an absent one is
    // inserted, so no stored field moves.
    let mut written: Vec<bool> = set
        .iter()
        .map(|(number, _)| fields.iter().any(|(n, ..)| n == number))
        .collect();
    let mut rewritten = vec![false; set.len()];
    let mut at = 0;
    for (number, value, start, end) in fields {
        out.extend_from_slice(&message[at..start]);
        at = end;
        for (index, (missing, value)) in set.iter().enumerate() {
            if !written[index] && *missing < number && *value != 0 {
                uint(*missing, *value, &mut out);
                written[index] = true;
            }
        }
        let Some(index) = set.iter().position(|(n, _)| *n == number) else {
            out.extend_from_slice(&message[start..end]);
            continue;
        };
        if !matches!(value, Value::Varint(_)) {
            return None;
        }
        if !std::mem::replace(&mut rewritten[index], true) {
            uint(number, set[index].1, &mut out);
        }
    }
    out.extend_from_slice(&message[at..]);
    for (index, (number, value)) in set.iter().enumerate() {
        if !written[index] && *value != 0 {
            uint(*number, *value, &mut out);
        }
    }
    Some(out)
}

/// A stored `PartDef` with Part Setup's options (layout-model.md 4.3,
/// 10.15): `flags` (2) and `pageBreakAfter` (3) replaced in place, or added
/// in field number order when missing and not 0, and, given
/// `break_field_key`, `breakFieldKey` (6) replaced in place or added in
/// field number order. Every other field and byte stays as stored. `None`
/// when the definition does not read or one of those fields is in another
/// wire type.
pub fn with_part_options(
    part_def: &[u8],
    flags: u64,
    page_break_after: u64,
    break_field_key: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let def = with_varints(part_def, "PartDef", &[(2, flags), (3, page_break_after)])?;
    let Some(key) = break_field_key else {
        return Some(def);
    };
    let fields = spans(&def, "PartDef")?;
    let mut written = fields.iter().any(|(n, ..)| *n == 6);
    let mut rewritten = false;
    let mut out = Vec::with_capacity(def.len() + key.len() + 2);
    let mut at = 0;
    for (number, value, start, end) in fields {
        out.extend_from_slice(&def[at..start]);
        at = end;
        if number == 6 {
            if !matches!(value, Value::Bytes(_)) {
                return None;
            }
            if !std::mem::replace(&mut rewritten, true) {
                bytes(6, key, &mut out);
            }
            continue;
        }
        if !written && number > 6 {
            bytes(6, key, &mut out);
            written = true;
        }
        out.extend_from_slice(&def[start..end]);
    }
    out.extend_from_slice(&def[at..]);
    if !written {
        bytes(6, key, &mut out);
    }
    Some(out)
}

/// The `breakFieldKey` of a sub-summary sorted by field `field` (its
/// catalog id) of occurrence `occurrence` (its catalog id) in this file, as
/// `Part::break_field` reads it: a counted key holding the counted segment
/// list `[file][occurrence][field]`, the file segment empty
/// (layout-model.md 4.3).
pub fn break_field_key(occurrence: u64, field: u64) -> Vec<u8> {
    let occurrence = om_hbam::slot::uint_segment(occurrence);
    let field = om_hbam::slot::uint_segment(field);
    let mut list = vec![0];
    for segment in [&occurrence, &field] {
        list.push(segment.len() as u8);
        list.extend_from_slice(segment);
    }
    let mut key = vec![list.len() as u8 + 1, list.len() as u8];
    key.extend(list);
    key
}

/// A stored `LayoutObjectStore` without objects `keys`: each one's
/// `objectDef` (field 2) and its root key (a field 1 holding it) removed,
/// every other field and byte kept as stored, in its place (layout-model.md
/// 5.1). `None` when the store does not read, or when a key is not exactly
/// one object and one root key, so only top-level objects are removed.
pub fn without_objects(store: &[u8], keys: &[u64]) -> Option<Vec<u8>> {
    let fields = spans(store, "LayoutObjectStore")?;
    let mut drop = vec![false; fields.len()];
    for &key in keys {
        let mut objects = 0;
        let mut roots = 0;
        for (index, (number, value, ..)) in fields.iter().enumerate() {
            let is_it = match (number, value) {
                (1, Value::Varint(root)) => {
                    let found = *root == key;
                    roots += usize::from(found);
                    found
                }
                (2, Value::Bytes(object)) => {
                    let found = spans(object, "LayoutObject")?
                        .iter()
                        .any(|(n, v, ..)| *n == 2 && *v == Value::Varint(key));
                    objects += usize::from(found);
                    found
                }
                _ => false,
            };
            drop[index] |= is_it;
        }
        if objects != 1 || roots != 1 {
            return None;
        }
    }
    let mut out = Vec::with_capacity(store.len());
    for ((.., start, end), dropped) in fields.iter().zip(drop) {
        if !dropped {
            out.extend_from_slice(&store[*start..*end]);
        }
    }
    Some(out)
}

/// Style object types of a layout's background and box, and of a body
/// part and its alternate rows (layout-model.md 4.3, 6.2).
pub const LAYOUT_BACKGROUND_STYLE: u64 = 20;
pub const LAYOUT_BOX_STYLE: u64 = 21;
pub const BODY_STYLE: u64 = 34;
pub const BODY_ALT_STYLE: u64 = 35;

/// `PartDef.type` of a header and a body part (layout-model.md 4.3), and
/// the header's style object type.
pub const HEADER: u64 = 1;
pub const BODY: u64 = 4;
pub const HEADER_STYLE: u64 = 32;

/// The `tableGridStyle` FileMaker writes for a new layout given none: the
/// table view's grid as FMUpgradeTool stored it (layout-model.md 10.6),
/// the same as the starter solutions' Form layouts store.
const DEFAULT_TABLE_GRID_STYLE: [u8; 219] = [
    0x0a, 0xd6, 0x01, 0x0a, 0xd3, 0x01, 0x08, 0x01, 0x10, 0x01, 0x1a, 0xcc, 0x01, 0x0a, 0x18, 0x08,
    0x14, 0x2a, 0x14, 0x0d, 0x00, 0x00, 0x00, 0x3f, 0x15, 0x00, 0x00, 0x00, 0x3f, 0x1d, 0x00, 0x00,
    0x00, 0x3f, 0x25, 0x00, 0x00, 0x80, 0x3f, 0x0a, 0x18, 0x08, 0x15, 0x2a, 0x14, 0x0d, 0x00, 0x00,
    0x00, 0x3f, 0x15, 0x00, 0x00, 0x00, 0x3f, 0x1d, 0x00, 0x00, 0x00, 0x3f, 0x25, 0x00, 0x00, 0x80,
    0x3f, 0x0a, 0x18, 0x08, 0x16, 0x2a, 0x14, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x15, 0x00, 0x00, 0x00,
    0x00, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x25, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x18, 0x08, 0x17, 0x2a,
    0x14, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x15, 0x00, 0x00, 0x00, 0x00, 0x1d, 0x00, 0x00, 0x00, 0x00,
    0x25, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x04, 0x08, 0x18, 0x18, 0x05, 0x0a, 0x04, 0x08, 0x19, 0x18,
    0x05, 0x0a, 0x04, 0x08, 0x1a, 0x18, 0x01, 0x0a, 0x04, 0x08, 0x1b, 0x18, 0x01, 0x0a, 0x11, 0x08,
    0x1c, 0x32, 0x0d, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xd0, 0x3f, 0x10, 0x06, 0x18, 0x00,
    0x0a, 0x11, 0x08, 0x1d, 0x32, 0x0d, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xd0, 0x3f, 0x10,
    0x06, 0x18, 0x00, 0x0a, 0x11, 0x08, 0x1e, 0x32, 0x0d, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x10, 0x06, 0x18, 0x00, 0x0a, 0x11, 0x08, 0x1f, 0x32, 0x0d, 0x09, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x06, 0x18, 0x00, 0x18, 0x00,
];

/// The `ObjectStyle` of only an object type, without local styles: what
/// FileMaker writes for a layout's background and box, and a part, given no
/// style (layout-model.md 10.6).
fn type_style(object_type: u64) -> Vec<u8> {
    let mut out = Vec::new();
    uint(3, object_type, &mut out);
    out
}

/// The `LayoutDef` FileMaker writes for a new layout given only its table
/// occurrence and width (layout-model.md 4.1, 10.6): no menu set, the
/// theme's background and box, the default table grid, `printColumnCount`
/// 2, 36-point margins, portrait, not responsive.
pub fn new_layout_def(table_alias_key: u64, base_width: f64) -> Vec<u8> {
    let mut out = Vec::new();
    uint(2, table_alias_key, &mut out);
    uint(3, 0, &mut out);
    bytes(5, &type_style(LAYOUT_BACKGROUND_STYLE), &mut out);
    bytes(6, &type_style(LAYOUT_BOX_STYLE), &mut out);
    bytes(7, &DEFAULT_TABLE_GRID_STYLE, &mut out);
    double(8, base_width, &mut out);
    double(9, 0.0, &mut out);
    double(10, 0.0, &mut out);
    uint(11, 2, &mut out);
    let margins = Rect {
        top: 36.0,
        left: 36.0,
        bottom: 36.0,
        right: 36.0,
        other: Vec::new(),
    };
    bytes(12, &rect(&margins), &mut out);
    double(13, 0.0, &mut out);
    double(14, 0.0, &mut out);
    uint(15, 0, &mut out);
    uint(16, 1, &mut out);
    uint(17, 0, &mut out);
    out
}

/// The `PartDef` FileMaker writes for a body part given only its height,
/// at the layout's top (layout-model.md 4.3, 10.6): no flags or page
/// break, styled by the theme's body and alternate body styles.
pub fn new_body_part(size: f64) -> Vec<u8> {
    part_def(BODY, 0, 0.0, size, BODY_STYLE, BODY_ALT_STYLE)
}

/// A `PartDef` of part type `kind` with `flags`, `offset` and `size`
/// points from the layout's top, no page break, styled by the theme's
/// styles for object types `style` and `alt_style` (layout-model.md 4.3).
pub fn part_def(
    kind: u64,
    flags: u64,
    offset: f64,
    size: f64,
    style: u64,
    alt_style: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    uint(1, kind, &mut out);
    uint(2, flags, &mut out);
    uint(3, 0, &mut out);
    double(4, offset, &mut out);
    double(5, size, &mut out);
    bytes(10, &type_style(style), &mut out);
    bytes(11, &type_style(alt_style), &mut out);
    out
}

/// A layout's entry in the layout catalog's index `[4, 1, 07, L, 02]`:
/// its table occurrence and menu set (layout-model.md 10.6).
pub fn layout_entry(table_alias_key: u64, menu_set_key: u64) -> Vec<u8> {
    let mut out = Vec::new();
    uint(2, table_alias_key, &mut out);
    uint(3, menu_set_key, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayoutObject, ObjectKind, ObjectStore};

    fn frame(top: f64, left: f64, bottom: f64, right: f64) -> Rect {
        Rect {
            top,
            left,
            bottom,
            right,
            other: Vec::new(),
        }
    }

    fn unhex(text: &str) -> Vec<u8> {
        let text: String = text.split_whitespace().collect();
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Layout Setup's fixed page margins replace `customMargins` (field 12)
    /// in place, its four doubles top, left, bottom and right, as FileMaker
    /// Pro 26.0.3 rewrote them for top, bottom, left and right 50 / 60 / 70
    /// / 80 points in #609's runs (layout-model.md 4.2); the rest of the
    /// definition stays as stored. A definition without them gets them at
    /// its end; one holding them twice is refused.
    #[test]
    fn fixed_page_margins_replace_the_stored_rect_as_filemaker_pro_does() {
        // The Rect FileMaker Pro wrote.
        let pro = unhex(
            "09 0000000000004940 11 0000000000805140 19 0000000000004e40 21 0000000000005440",
        );
        let margins = frame(50.0, 70.0, 60.0, 80.0);
        let def = |margins: &[u8]| {
            let mut def = Vec::new();
            double(8, 1024.0, &mut def);
            uint(11, 2, &mut def);
            bytes(CUSTOM_MARGINS_FIELD, margins, &mut def);
            uint(17, 0, &mut def);
            def
        };
        let stored = def(&rect(&frame(36.0, 36.0, 36.0, 36.0)));
        assert_eq!(with_custom_margins(&stored, &margins), Some(def(&pro)));
        let mut bare = Vec::new();
        double(8, 1024.0, &mut bare);
        let mut added = bare.clone();
        bytes(CUSTOM_MARGINS_FIELD, &pro, &mut added);
        assert_eq!(with_custom_margins(&bare, &margins), Some(added));
        let mut twice = stored.clone();
        bytes(CUSTOM_MARGINS_FIELD, &pro, &mut twice);
        assert_eq!(with_custom_margins(&twice, &margins), None);
    }

    #[test]
    fn moving_an_object_rewrites_only_its_bounds() {
        // Two objects and an unknown store field: object 9's frame changes
        // in place, everything else keeps its bytes.
        let first = field_object(9, &frame(112.0, 366.0, 156.0, 632.0), 1_065_090, 2);
        let second = text_object(10, &frame(10.0, 10.0, 30.0, 100.0));
        let mut store = Vec::new();
        uint(1, 9, &mut store);
        uint(1, 10, &mut store);
        bytes(2, &first, &mut store);
        uint(77, 5, &mut store);
        bytes(2, &second, &mut store);
        let moved = frame(120.0, 300.0, 164.0, 600.0);
        let after = with_object_bounds(&store, 9, &moved).unwrap();
        assert_eq!(after.len(), store.len());
        let decoded = ObjectStore::decode(&after).unwrap();
        let objects: Vec<&LayoutObject> = decoded.objects.iter().collect();
        assert_eq!(objects[0].bounds.as_ref().unwrap(), &moved);
        assert_eq!(
            objects[1].bounds,
            ObjectStore::decode(&store).unwrap().objects[1].bounds
        );
        // Only the 32 bytes of the four edges differ.
        let differing = store.iter().zip(&after).filter(|(a, b)| a != b).count();
        assert!(differing <= 32, "{differing}");
        assert_eq!(after[store.len() - second.len()..], second[..]);
        assert!(with_object_bounds(&store, 11, &moved).is_none());
    }

    #[test]
    fn a_control_is_set_as_pro_writes_it_and_a_misread_style_is_refused() {
        // As Pro's Inspector writes it (layout-model.md 10.13): the value
        // list key the object lacks goes in field number order, between
        // displayType (5) and keyboardType (7); the flags stay as given.
        let object = field_object(9, &frame(112.0, 366.0, 156.0, 632.0), 1_065_090, 2);
        let mut store = Vec::new();
        uint(1, 9, &mut store);
        bytes(2, &object, &mut store);
        let control = FieldControl {
            flags: EDIT_BOX_FLAGS,
            display_type: 2,
            value_list_key: 3,
            style_type: 63,
        };
        let after = with_field_control(&store, 9, &control).unwrap();
        let decoded = ObjectStore::decode(&after).unwrap();
        let field = decoded.objects[0].field_obj.as_ref().unwrap();
        assert_eq!((field.display_type, field.value_list_key), (2, 3));
        assert_eq!(u64::from(field.flags), EDIT_BOX_FLAGS);
        assert!(after.windows(6).any(|w| w == [0x28, 2, 0x30, 3, 0x38, 1]));
        // A style field 2 that is no string is not understood: refused, not
        // dropped.
        let mut style = Vec::new();
        uint(2, 7, &mut style);
        uint(3, 61, &mut style);
        let mut field = Vec::new();
        uint(5, 0, &mut field);
        let mut odd = Vec::new();
        uint(1, u64::from(FIELD), &mut odd);
        uint(2, 9, &mut odd);
        bytes(5, &style, &mut odd);
        bytes(8, &field, &mut odd);
        let mut store = Vec::new();
        bytes(2, &odd, &mut store);
        assert!(with_field_control(&store, 9, &control).is_none());
    }

    #[test]
    fn part_options_go_in_field_order_and_a_break_field_key_reads_back() {
        // The Tasks starter's Task List sub-summary, sorted by field 4 of
        // occurrence `d0 00 02` (layout-model.md 4.3).
        let key = break_field_key(0xd0_0002, 4);
        assert_eq!(key, unhex("08 07 00 03 d0 00 02 01 04"));
        // A sub-summary without options: type, offset, size, a style (10).
        let mut def = Vec::new();
        uint(1, 3, &mut def);
        double(4, 74.0, &mut def);
        double(5, 30.0, &mut def);
        bytes(10, &[0x18, 0x64], &mut def);
        let after = with_part_options(&def, 0x41, 3, Some(&key)).unwrap();
        let part = crate::Part::decode(2, &after).unwrap();
        assert_eq!((part.flags, part.page_break_after), (0x41, 3));
        let field = part.break_field().unwrap().unwrap();
        assert_eq!((field.occurrence, field.field), (0xd0_0002, 4));
        // In field number order: 1, 2, 3, then 4 and 5 as stored, the key
        // (6) before the style (10), which keeps its bytes.
        assert!(after.starts_with(&[0x08, 0x03, 0x10, 0x41, 0x18, 0x03]));
        assert!(after.ends_with(&[&[0x32, 9][..], &key, &[0x52, 2, 0x18, 0x64]].concat()));
        // Back to no options: the flags are written as 0 where they stand.
        let back = with_part_options(&after, 0, 0, None).unwrap();
        assert_eq!(crate::Part::decode(2, &back).unwrap().flags, 0);
        assert_eq!(back.len(), after.len());
    }

    #[test]
    fn deleting_an_object_removes_its_definition_and_root_key_only() {
        let first = field_object(9, &frame(112.0, 366.0, 156.0, 632.0), 1_065_090, 2);
        let second = text_object(10, &frame(10.0, 10.0, 30.0, 100.0));
        let mut store = Vec::new();
        uint(1, 9, &mut store);
        uint(1, 10, &mut store);
        bytes(2, &first, &mut store);
        uint(77, 5, &mut store);
        bytes(2, &second, &mut store);
        let after = without_objects(&store, &[9]).unwrap();
        let mut expected = Vec::new();
        uint(1, 10, &mut expected);
        uint(77, 5, &mut expected);
        bytes(2, &second, &mut expected);
        assert_eq!(after, expected);
        assert!(without_objects(&store, &[11]).is_none());
    }

    #[test]
    fn a_new_layout_is_what_filemaker_writes_given_its_occurrence_width_and_body() {
        // FMUpgradeTool 26.0.3 adding a layout on the Contacts starter's
        // occurrence `d0 00 02`, 1024 points wide, with a 300-point body
        // and no style (layout-model.md 10.6): `[4, 5, L, 02]`, the body's
        // `[4, 5, L, 03, 01, 02]` and `[4, 1, 07, L, 02]`.
        let def = concat!(
            "1082814118002a021814320218153adb010ad6010ad301080110011acc010a18",
            "08142a140d0000003f150000003f1d0000003f250000803f0a1808152a140d00",
            "00003f150000003f1d0000003f250000803f0a1808162a140d00000000150000",
            "00001d0000000025000000000a1808172a140d0000000015000000001d000000",
            "0025000000000a04081818050a04081918050a04081a18010a04081b18010a11",
            "081c320d09000000000000d03f100618000a11081d320d09000000000000d03f",
            "100618000a11081e320d090000000000000000100618000a11081f320d090000",
            "0000000000001006180018004100000000000090404900000000000000005100",
            "0000000000000058026224090000000000004240110000000000004240190000",
            "0000000042402100000000000042406900000000000000007100000000000000",
            "007800800101880100",
        );
        assert_eq!(new_layout_def(1_065_090, 1024.0), unhex(def));
        assert_eq!(
            new_body_part(300.0),
            unhex("0804 1000 1800 210000000000000000 290000000000c07240 52021822 5a021823")
        );
        assert_eq!(layout_entry(1_065_090, 0), unhex("108281411800"));
    }

    #[test]
    fn a_field_object_is_the_templates_edit_box() {
        // The Contacts template's object 9, an edit box on `d0 00 02` field
        // 2 (layout-model.md 5.4), byte for byte but for its two flag
        // words: the template's carry the tooltip (`0x4000`) and
        // placeholder bits, `80 80 81 80 03` and `e0 81 48`.
        let bytes = field_object(9, &frame(112.0, 366.0, 156.0, 632.0), 1_065_090, 2);
        let template = [
            "0801",
            "1009",
            "188080808003",
            "2224",
            "09",
            "000000000000 5c40",
            "11",
            "0000000000e0 7640",
            "19",
            "000000000080 6340",
            "21",
            "0000000000c0 8340",
            "2a13",
            "0a0f0a0d080110011a070a0508e5041800",
            "183d",
            "4217",
            "08e001",
            "1208",
            "08828141",
            "1002",
            "1801",
            "1801",
            "2000",
            "2800",
            "3801",
            "4001",
        ]
        .concat()
        .replace(' ', "");
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, template);
        let object = LayoutObject::decode(&bytes).unwrap();
        assert_eq!(object.kind(), ObjectKind::Field);
        assert_eq!(object.id, 9);
        assert_eq!(object.flags, ANCHOR_LEFT_TOP);
        let spec = object.field_spec().unwrap();
        assert_eq!(
            (spec.table_key, spec.field_key, spec.repetition),
            (1_065_090, 2, 1)
        );
        let field = object.field_obj.as_ref().unwrap();
        assert_eq!((field.num_of_reps, field.display_type), (1, 0));
        assert!(field.other.is_empty() && object.other.is_empty());
        assert_eq!(object.style.as_ref().unwrap().object_type_id, 61);
        assert_eq!(object.bounds.unwrap().right, 632.0);
    }

    #[test]
    fn objects_are_appended_without_rewriting_the_store() {
        let mut stored = Vec::new();
        uint(1, 1, &mut stored);
        bytes(
            2,
            &text_object(1, &frame(10.0, 10.0, 30.0, 100.0)),
            &mut stored,
        );
        // A field this decoder does not know stays as stored.
        uint(9, 77, &mut stored);
        let field = field_object(2, &frame(10.0, 120.0, 30.0, 300.0), 1_065_090, 1);
        let label = text_object(3, &frame(10.0, 310.0, 30.0, 400.0));
        let store = append_to_store(&stored, &[3, 2], &[label, field]);
        assert_eq!(store[..stored.len()], stored[..]);
        let decoded = ObjectStore::decode(&store).unwrap();
        assert_eq!(decoded.root, [1, 3, 2]);
        let ids: Vec<u64> = decoded.objects.iter().map(|o| o.id).collect();
        assert_eq!(ids, [1, 3, 2]);
        assert_eq!(decoded.other.len(), 1);
        assert_eq!(decoded.get(3).unwrap().kind(), ObjectKind::Text);
        assert_eq!(append_to_store(&[], &[5], &[]), [0x08, 5]);
    }
}
