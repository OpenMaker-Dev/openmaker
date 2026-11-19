//! Placing a portal on a layout's body (#227, authoring operation 21): a
//! portal showing rows of a related table occurrence, with fields of that
//! occurrence in its first row, as FileMaker Pro 26.0.3 stores one drawn
//! with the Portal tool, Portal Setup's defaults and Add Fields to Portal
//! (layout-model.md 10.17).

use super::*;

/// The most rows and fields a placed portal has (OpenMaker's limits; the
/// starters' portals show 2 to 5 rows).
pub const MAX_PORTAL_ROWS: u32 = 100;
pub const MAX_PORTAL_FIELDS: usize = 20;

/// Where FileMaker Pro put the one field it added to a new portal, and its
/// size: 4 points inside the portal's top and left (relative to the
/// portal), 249 by 31 points (layout-model.md 10.17).
const ROW_INSET: f64 = 4.0;
const FIELD_WIDTH: f64 = 249.0;
const FIELD_HEIGHT: f64 = 31.0;
/// Between fields side by side, and the smallest field OpenMaker makes
/// (OpenMaker's choices: Pro's twin has one field).
const FIELD_GAP: f64 = 4.0;
const MIN_FIELD: f64 = 8.0;

/// The value of a portal's field reference `01, <occurrence>, 00, <portal>`:
/// on all 10 portals of the starters and in Pro's twin (layout-model.md
/// 10.17).
const PORTAL_REFERENCE: u8 = 0x09;

/// A layout member's reference to its theme `[6, 5, T]` (layout-model.md
/// 4.1).
const THEME: u8 = 0x0b;

/// A portal to place on a layout's body.
#[derive(Clone, Debug, PartialEq)]
pub struct PortalPlacement {
    /// The table occurrence whose related records the portal shows (its
    /// catalog id): not the layout's own.
    pub occurrence: u64,
    /// The portal's frame in layout coordinates (points).
    pub frame: Rect,
    /// How many rows it shows.
    pub rows: u32,
    /// The fields of `occurrence` in its first row, left to right (their
    /// catalog ids).
    pub fields: Vec<u64>,
}

/// A portal placement asked of the open file's provider
/// ([`crate::DataProvider::place_layout_portal`]) on layout `layout`.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutPortalPlacement {
    pub layout: u32,
    /// The base table the fields were chosen from: the table the portal's
    /// occurrence showed then. Checked again by the host, as for a field
    /// ([`LayoutFieldPlacement::table`]).
    pub table: u64,
    pub portal: PortalPlacement,
    /// The layout's revision the placement was made against.
    pub expected: Revision,
}

/// A portal the host placed, or found already placed by an earlier request
/// whose answer was lost (`changed` false).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedLayoutPortal {
    pub changed: bool,
    pub portal: u64,
    /// Its fields' object keys, in the order asked.
    pub fields: Vec<u64>,
    pub revision: Revision,
}

/// A portal planned into a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedPortal {
    /// The portal's key, the next after every key the layout uses, and its
    /// fields' keys after it.
    pub portal: u64,
    pub fields: Vec<u64>,
    /// The part it is in, and its place in the root stack (on top).
    pub part: u64,
    pub stacking: usize,
    /// The stored objects it overlaps (they stay beneath it).
    pub overlaps: Vec<u64>,
    /// The layout member `[4, 5, L]` with the portal, without UUIDs or
    /// stamps.
    pub member: OwnedNode,
    /// What changed in the member, as [`Planned::changes`].
    pub changes: Vec<(String, String)>,
}

/// The frames of a portal's fields relative to the portal: side by side in
/// its first row, 4 points in, each as wide as FileMaker Pro made its one
/// field where they fit and narrower where they do not, and as high where
/// the row allows (whole points). Refused when a field would be smaller
/// than [`MIN_FIELD`].
pub fn portal_field_frames(placement: &PortalPlacement) -> Result<Vec<Rect>, Refusal> {
    let count = placement.fields.len();
    if count == 0 || count > MAX_PORTAL_FIELDS {
        return Err(Refusal::Invalid(format!(
            "A portal is placed with 1 to {MAX_PORTAL_FIELDS} fields."
        )));
    }
    if placement.rows == 0 || placement.rows > MAX_PORTAL_ROWS {
        return Err(Refusal::Invalid(format!(
            "A portal shows 1 to {MAX_PORTAL_ROWS} rows."
        )));
    }
    let frame = &placement.frame;
    let row = frame.height() / f64::from(placement.rows);
    let height = FIELD_HEIGHT.min(row - 2.0 * ROW_INSET).floor();
    let width = ((frame.width() - 2.0 * ROW_INSET - FIELD_GAP * (count - 1) as f64) / count as f64)
        .min(FIELD_WIDTH)
        .floor();
    if !(height >= MIN_FIELD && width >= MIN_FIELD) {
        return Err(Refusal::Invalid(format!(
            "The portal's rows ({row:.1} points high) or its width ({} points) leave no room \
             for {count} field{} of at least {MIN_FIELD} points.",
            frame.width(),
            if count == 1 { "" } else { "s" }
        )));
    }
    Ok((0..count)
        .map(|i| {
            let left = ROW_INSET + i as f64 * (width + FIELD_GAP);
            Rect {
                top: ROW_INSET,
                left,
                bottom: ROW_INSET + height,
                right: left + width,
                other: Vec::new(),
            }
        })
        .collect())
}

/// The key of the theme layout `member` names (`[4, 5, L, 0b]`, a
/// count-prefixed or plain unsigned integer; layout-model.md 4.1).
pub fn theme_key(member: &OwnedNode) -> Option<u64> {
    let reference = member.child(&[THEME])?.value.as_deref()?;
    match reference.split_first() {
        Some((&len, rest)) if usize::from(len) == rest.len() => be_uint(rest),
        _ => be_uint(reference),
    }
}

/// How far the rows of a new portal sit inside it on layout `member`, whose
/// theme is `theme` (`[6, 5, T]`, if it has one): the portal's drawn left
/// and top border widths ([`om_layout::Layout::portal_row_inset`]).
fn row_inset(member: &OwnedNode, theme: Option<&OwnedNode>, portal: &[u8]) -> Option<(f64, f64)> {
    let id = be_uint(&member.segment)?;
    let mut catalogs = vec![OwnedNode::list(
        vec![4],
        vec![OwnedNode::list(vec![5], vec![member.clone()])],
    )];
    if let Some(theme) = theme {
        catalogs.push(OwnedNode::list(
            vec![6],
            vec![OwnedNode::list(vec![5], vec![theme.clone()])],
        ));
    }
    let wire = OwnedNode::list(Vec::new(), catalogs).to_wire()?;
    let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&wire)).ok()?;
    let layout = om_layout::Layout::decode_from_tree(&tree, id).ok()??;
    let portal = LayoutObject::decode(portal).ok()?;
    Some(layout.portal_row_inset(&portal))
}

/// Plan `placement` into `member`: a portal on top of the body's objects,
/// its fields as its contents, with their attribute records, field
/// references, object index and tab order entries, the portal's entry in
/// the body's list, a column per field in the stored table view, and the
/// last key issued raised (layout-model.md 10.17). `theme` is the layout's
/// theme `[6, 5, T]` ([`theme_key`]), which places the fields' index
/// entries inside the portal's borders.
pub fn plan_portal(
    member: &OwnedNode,
    placement: &PortalPlacement,
    theme: Option<&OwnedNode>,
) -> Result<PlannedPortal, Refusal> {
    let def = placeable(member)?;
    if om_layout::occurrence_id(def.table_alias_key) == Some(placement.occurrence) {
        return Err(Refusal::Unsupported(
            "The occurrence is the layout's own; a portal shows the records of a related \
             occurrence."
                .into(),
        ));
    }
    let Some(table_key) = serial_value(placement.occurrence) else {
        return Err(Refusal::Invalid(
            "The occurrence id is not a catalog key.".into(),
        ));
    };
    let field_keys = placement
        .fields
        .iter()
        .map(|&field| serial_value(field))
        .collect::<Option<Vec<u64>>>()
        .ok_or_else(|| Refusal::Invalid("A field id is not a catalog key.".into()))?;
    let field_frames = portal_field_frames(placement)?;
    let frame = &placement.frame;
    check_frame(frame, def.base_width, "portal")?;
    let OnBody {
        body,
        stored,
        objects,
        overlaps,
    } = on_body(member, &[("portal", frame)])?;

    // The new objects: the portal, then its fields, after every key the
    // layout uses.
    let issued = last_issued(member)?;
    let portal = first_new_key(member, &objects)?;
    let mut fields = Vec::with_capacity(field_keys.len());
    let mut key = portal;
    for _ in &field_keys {
        key = next_object_key(key)?;
        fields.push(key);
    }
    let last = key;

    // FileMaker Pro stores the fields before the portal; only the portal
    // goes on the root stack.
    let mut new_objects: Vec<Vec<u8>> = fields
        .iter()
        .zip(&field_keys)
        .zip(&field_frames)
        .map(|((&key, &field_key), bounds)| {
            encode::portal_field_object(key, bounds, table_key, field_key)
        })
        .collect();
    let portal_object = encode::portal_object(portal, frame, &fields, table_key, placement.rows);
    let (inset_left, inset_top) = row_inset(member, theme, &portal_object).ok_or_else(|| {
        Refusal::Unsupported(
            "OpenMaker cannot read the layout with its theme to place a portal's fields.".into(),
        )
    })?;
    new_objects.push(portal_object);
    let store = encode::append_to_store(&stored, &[portal], &new_objects);

    let mut next = member.clone();
    let mut changes = Vec::new();
    next.set_child(stored_value(&[OBJECTS], &store));
    changes.push((
        "[4, 5, L, 07]".to_owned(),
        format!(
            "the object store: portal {portal} on top of the root stack with field objects {} \
             as its contents ({} bytes appended to the {} stored, which are kept)",
            keys_text(&fields),
            store.len() - stored.len(),
            stored.len()
        ),
    ));

    // Attribute records, and the last key issued.
    for &key in std::iter::once(&portal).chain(&fields) {
        let record = attributes(key, None);
        changes.push((
            format!("[4, 5, L, 05, {}]", hex(&record.segment)),
            format!("object {key}'s attributes: no name, and the UUIDs the host makes"),
        ));
        next.put(&[&[ATTRIBUTES], &record.segment.clone()], record);
    }
    let mut counter = member
        .at(&[&[ATTRIBUTES], &[LAST_KEY]])
        .cloned()
        .unwrap_or_else(|| OwnedNode::leaf(Vec::new(), Vec::new()));
    counter.value = Some(key_string(last));
    next.put(&[&[ATTRIBUTES], &[LAST_KEY]], counter);
    changes.push((
        "[4, 5, L, 05, 00]".to_owned(),
        format!(
            "the last object key issued: {last} (was {})",
            issued.map_or_else(|| "none".into(), |k| k.to_string())
        ),
    ));

    // Field references: the portal's, under no field, and each field's.
    let occurrence_segment = uint_segment(placement.occurrence);
    next.put(
        &[
            &[FIELD_INDEX],
            &occurrence_segment,
            &[0],
            &serial::encode(portal),
        ],
        OwnedNode::leaf(Vec::new(), vec![PORTAL_REFERENCE]),
    );
    changes.push((
        format!(
            "[4, 5, L, 01, {}, 00, {}]",
            hex(&occurrence_segment),
            hex(&serial::encode(portal))
        ),
        format!(
            "the field reference index: portal {portal} shows occurrence {}",
            placement.occurrence
        ),
    ));
    for (&key, &field) in fields.iter().zip(&placement.fields) {
        let field_segment = uint_segment(field);
        next.put(
            &[
                &[FIELD_INDEX],
                &occurrence_segment,
                &field_segment,
                &serial::encode(key),
            ],
            OwnedNode::leaf(Vec::new(), Vec::new()),
        );
        changes.push((
            format!(
                "[4, 5, L, 01, {}, {}, {}]",
                hex(&occurrence_segment),
                hex(&field_segment),
                hex(&serial::encode(key))
            ),
            format!(
                "the field reference index: field {field} of occurrence {} shown by object {key}",
                placement.occurrence
            ),
        ));
    }

    // The object index: the portal's entry, as the last root, holds no
    // value; its fields' entries are nested below it at the places they
    // are drawn, inside the portal's borders.
    let sequence = tree_size(&objects)?;
    if largest_indexed(member).is_some_and(|largest| largest >= sequence) {
        return Err(Refusal::Unsupported(
            "The layout's object index does not match its objects; OpenMaker does not place \
             objects on it."
                .into(),
        ));
    }
    let (portal_index, _) = object_index_entry(frame.top, frame.left, sequence, portal)?;
    let mut index = next
        .child(&[OBJECT_INDEX])
        .cloned()
        .unwrap_or_else(|| OwnedNode::list(vec![OBJECT_INDEX], Vec::new()));
    if index.child(&portal_index).is_some() {
        return Err(Refusal::Unsupported(
            "The layout's object index already has an entry at the portal's place.".into(),
        ));
    }
    let mut portal_entry = OwnedNode::list(portal_index.clone(), Vec::new());
    let mut tab = next
        .child(&[TAB_ORDER])
        .cloned()
        .unwrap_or_else(|| OwnedNode::list(vec![TAB_ORDER], Vec::new()));
    let mut order = tab
        .children
        .iter()
        .map(|e| <[u8; 2]>::try_from(e.segment.as_slice()).map(u16::from_be_bytes))
        .try_fold(0u16, |most, order| order.map(|o| most.max(o)))
        .map_err(|_| {
            Refusal::Unsupported("The layout's tab order is not one OpenMaker reads.".into())
        })?;
    for (i, (&key, bounds)) in fields.iter().zip(&field_frames).enumerate() {
        let (entry_key, value) = object_index_entry(
            frame.top + bounds.top + inset_top,
            frame.left + bounds.left + inset_left,
            sequence + 1 + i,
            key,
        )?;
        portal_entry.set_child(OwnedNode::leaf(entry_key.clone(), value.clone()));
        changes.push((
            format!("[4, 5, L, 0a, {}, {}]", hex(&portal_index), hex(&entry_key)),
            format!("the object index: field object {key} in portal {portal}"),
        ));
        order = order
            .checked_add(1)
            .ok_or_else(|| Refusal::Unsupported("The layout's tab order is full.".into()))?;
        tab.set_child(OwnedNode::leaf(order.to_be_bytes().to_vec(), value));
        changes.push((
            format!("[4, 5, L, 15, {}]", hex(&order.to_be_bytes())),
            format!("the tab order: field object {key}, after the objects in it before"),
        ));
    }
    index.set_child(portal_entry);
    next.set_child(index);
    next.set_child(tab);
    changes.push((
        format!("[4, 5, L, 0a, {}]", hex(&portal_index)),
        format!(
            "the object index: portal {portal} at top {}, left {}, pre-order index {sequence}",
            frame.top, frame.left
        ),
    ));

    // The body's list of its top-level objects gains the portal.
    let unreadable_list = || {
        Refusal::Unsupported(
            "The body part has no list of its objects that OpenMaker reads; every part FileMaker \
             saves has one, and OpenMaker does not place objects without it."
                .into(),
        )
    };
    let body_node = part_node(member, body.id).ok_or_else(unreadable_list)?;
    let mut list_node = body_node
        .child(&[PART_OBJECTS])
        .ok_or_else(unreadable_list)?
        .clone();
    let stored_list = list_node.value.clone().ok_or_else(unreadable_list)?;
    let listed = part_list(&stored_list).ok_or_else(unreadable_list)?;
    let mut list = u32::try_from(listed.len() + 1)
        .map_err(|_| unreadable_list())?
        .to_be_bytes()
        .to_vec();
    list.extend_from_slice(&stored_list[4..]);
    list.extend(part_list_entry(portal));
    if list.len() > MAX_INLINE_VALUE {
        return Err(Refusal::Unsupported(
            "The body part lists too many objects for OpenMaker to add another.".into(),
        ));
    }
    list_node.value = Some(list);
    next.put(
        &[&[PARTS], &body_node.segment.clone(), &[PART_OBJECTS]],
        list_node,
    );
    changes.push((
        format!("[4, 5, L, 03, {}, 07]", hex(&body_node.segment)),
        format!(
            "the body part's list of its objects: portal {portal} added at its end ({} listed \
             before)",
            listed.len()
        ),
    ));

    // A column per field in the stored table view, as for a field placed
    // on the layout (#815).
    if let Some(view) = next.child(&[TABLE_VIEW]) {
        let format = column_format(view)?;
        let columns: Vec<NewColumn> = field_keys
            .iter()
            .zip(&placement.fields)
            .map(|(&field_key, &field)| NewColumn {
                keys: (table_key, field_key),
                occurrence: occurrence_segment.clone(),
                field: uint_segment(field),
            })
            .collect();
        let view = renew_columns(view, &|_| false, &columns)?;
        changes.push((
            "[4, 5, L, 0d]".to_owned(),
            "the stored table view: a column for each of the portal's fields, and a new empty \
             column after them in place of the one there was, each with the columns' data \
             format record"
                .to_owned(),
        ));
        next.set_child(view);
        // The portal's fields get the columns' data format record, as Pro
        // gave its field one (layout-model.md 10.17).
        for &key in &fields {
            next.put(
                &[&[ATTRIBUTES], &serial::encode(key), &[FORMAT]],
                OwnedNode::leaf(vec![FORMAT], format.clone()),
            );
        }
        changes.push((
            "[4, 5, L, 05, <field>, 0d]".to_owned(),
            format!(
                "the data format of field objects {}: the table view columns' record",
                keys_text(&fields)
            ),
        ));
    }

    Ok(PlannedPortal {
        portal,
        fields,
        part: body.id,
        stacking: objects.root.len(),
        overlaps,
        member: next,
        changes,
    })
}

/// The portal an earlier placement of `placement` made on layout `member`,
/// if it is on top of its root stack exactly as [`plan_portal`] makes it:
/// the portal and each of its fields as encoded (their setup, options and
/// frames; names, UUIDs and the like as the layout decodes them aside), its
/// fields keyed after it in order. Its key and its fields' keys. A portal
/// changed since, its options or a field's control included, is not.
pub fn already_placed_portal(
    member: &OwnedNode,
    placement: &PortalPlacement,
) -> Option<(u64, Vec<u64>)> {
    let layout = decode(member)?;
    let top = layout.objects.get(*layout.objects.root.last()?)?;
    let table_key = serial_value(placement.occurrence)?;
    let frames = portal_field_frames(placement).ok()?;
    let fields: Vec<u64> = (1..=placement.fields.len() as u64)
        .map(|i| top.id.checked_add(i))
        .collect::<Option<_>>()?;
    let made = encode::portal_object(top.id, &placement.frame, &fields, table_key, placement.rows);
    if LayoutObject::decode(&made).ok()? != strip(top) {
        return None;
    }
    for ((&key, &field), frame) in fields.iter().zip(&placement.fields).zip(&frames) {
        let object = layout.objects.get(key)?;
        let made = encode::portal_field_object(key, frame, table_key, serial_value(field)?);
        if LayoutObject::decode(&made).ok()? != strip(object) {
            return None;
        }
    }
    Some((top.id, fields))
}

fn keys_text(keys: &[u64]) -> String {
    keys.iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
