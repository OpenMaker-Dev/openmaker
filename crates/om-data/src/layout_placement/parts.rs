//! Adding, deleting and resizing a layout's parts (#227, authoring
//! operation 22), as FileMaker Pro 26.0.3's Part Setup (Create, Delete) and
//! a dragged part boundary change them (layout-model.md 10.18). Parts are
//! keyed 1 to n from the top, each part's subtree (its `PartDef` and its
//! list of objects) moving with it when the keys change; the parts below a
//! change move by it, and so do their objects.

use std::collections::HashMap;

use super::*;

/// The key, under a layout's parts `[4, 5, L, 03]`, of the last part key
/// (layout-model.md 10.18).
const LAST_PART: u8 = 0x00;

/// A change to a layout's parts.
#[derive(Clone, Debug, PartialEq)]
pub enum PartChange {
    /// A new part of type `part_type` (`PartDef.type`, layout-model.md 4.3)
    /// `size` points high, where FileMaker's order of parts puts it.
    Add { part_type: i32, size: f64 },
    /// Delete part `part` (its key), of type `part_type`.
    Delete { part: u64, part_type: i32 },
    /// Make part `part` (of type `part_type`) `size` points high.
    Resize {
        part: u64,
        part_type: i32,
        size: f64,
    },
}

/// A change to the parts of layout `layout` (its catalog id) asked of the
/// open file's provider ([`crate::DataProvider::set_layout_parts`]), made
/// on the layout at revision `expected`.
#[derive(Clone, Debug, PartialEq)]
pub struct PartsChange {
    pub layout: u32,
    pub expected: Revision,
    pub change: PartChange,
}

/// The part types this version adds: every type FileMaker has once on a
/// layout, but not sub-summaries, which need a break field.
pub const ADDABLE_PARTS: [i32; 9] = [12, 0, 1, 2, 4, 6, 7, 8, 13];

/// FileMaker's top-to-bottom order of part types (layout-model.md 4.3), as
/// a rank, 0 at the top: top navigation, title header, header, leading
/// grand summary, leading sub-summaries, body, trailing sub-summaries,
/// trailing grand summary, footer, title footer, bottom navigation. `None`
/// for a type it does not order.
pub fn part_rank(part_type: i32) -> Option<u8> {
    Some(match part_type {
        12 => 0,
        0 => 1,
        1 => 2,
        2 => 3,
        3 => 4,
        4 => 5,
        5 => 6,
        6 => 7,
        7 => 8,
        8 => 9,
        13 => 10,
        _ => return None,
    })
}

/// The style object types of a new part of `part_type`, and of its
/// alternate rows: the part's own type twice, but the body's alternate
/// rows (layout-model.md 4.3, 6.2; the starters' 66 parts, Pro's new
/// header in `227-parts/`).
fn part_styles(part_type: i32) -> Option<(u64, u64)> {
    Some(match part_type {
        0 => (31, 31),
        1 => (32, 32),
        2 => (33, 33),
        4 => (34, 35),
        6 => (37, 37),
        7 => (38, 38),
        8 => (39, 39),
        12 => (40, 40),
        13 => (41, 41),
        _ => return None,
    })
}

/// The name of a part type, for refusals.
pub fn part_type_name(part_type: i32) -> &'static str {
    match part_type {
        0 => "title header",
        1 => "header",
        2 => "leading grand summary",
        3 => "leading sub-summary",
        4 => "body",
        5 => "trailing sub-summary",
        6 => "trailing grand summary",
        7 => "footer",
        8 => "title footer",
        12 => "top navigation",
        13 => "bottom navigation",
        _ => "part of an unknown type",
    }
}

/// A part's size: whole points, at least 1 (OpenMaker's choice; FileMaker
/// Pro's smallest part is not measured).
fn check_size(size: f64) -> Result<(), Refusal> {
    if !(size.is_finite() && size >= 1.0 && size.fract() == 0.0 && size <= MAX_COORDINATE) {
        return Err(Refusal::Invalid(format!(
            "A part is 1 to {MAX_COORDINATE} whole points high."
        )));
    }
    Ok(())
}

/// A part of the new list: the stored part it was (its index in the stored
/// list), or a new one, its type and size.
struct Slot {
    from: Option<usize>,
    part_type: i32,
    size: f64,
}

/// Plan `change` into `member`: the parts keyed 1 to n again from the
/// top, each stored part's subtree moved with it and its offset (and for
/// a resize its size) rewritten in its `PartDef`, a new part's `PartDef`
/// with an empty list of objects, `03, 00` the last key, and every object
/// of a part that moved shifted by as much (its frame, and its object
/// index entries, nested ones included). Refused: a sub-summary, a second
/// part of a type FileMaker has once, deleting a part that holds objects
/// or the only part, shrinking a part above an object in it, and parts
/// this version does not re-key (not keyed 1 to n from the top, offsets
/// that do not follow each other, an object no part lists).
pub fn plan_parts(
    member: &OwnedNode,
    change: &PartChange,
) -> Result<crate::layout_objects::Planned, Refusal> {
    let unsupported = |why: String| Refusal::Unsupported(why);
    placeable(member)?;
    let mut stored = parts(member)?;
    stored.sort_by_key(|p| p.id);
    // Keyed 1 to n from the top, each part where the ones above end, with
    // a height; the parts node holds nothing else but the last key, so no
    // re-keyed part can land on a child OpenMaker does not read.
    let not_stacked = || {
        unsupported(
            "The layout's parts are not keyed and stacked as FileMaker Pro stores them; \
             OpenMaker does not change them."
                .into(),
        )
    };
    let others = member.child(&[PARTS]).map_or(0, |n| {
        n.children
            .iter()
            .filter(|c| c.segment != [LAST_PART])
            .count()
    });
    if others != stored.len() {
        return Err(not_stacked());
    }
    let mut offset = 0.0;
    for (i, part) in stored.iter().enumerate() {
        let sized = part.height.is_finite() && part.height >= 0.0;
        if !sized
            || part.id != i as u64 + 1
            || part.top != offset
            || part_rank(part.part_type).is_none()
        {
            return Err(not_stacked());
        }
        offset += part.height;
    }
    if offset.is_nan() || offset > MAX_COORDINATE {
        return Err(not_stacked());
    }
    let lists: Vec<Vec<u64>> = stored
        .iter()
        .map(|part| {
            let node = part_node(member, part.id).and_then(|n| n.child(&[PART_OBJECTS]));
            node.and_then(|n| part_list(n.value.as_deref()?))
                .ok_or_else(|| {
                    unsupported(format!(
                        "Part {} has no list of its objects that OpenMaker reads.",
                        part.id
                    ))
                })
        })
        .collect::<Result<_, _>>()?;
    let (stored_bytes, objects) = stored_objects(member)?;
    let geometry = om_layout::Layout {
        objects: objects.clone(),
        ..Default::default()
    };

    // Each top-level object is in one part's list, and inside that part:
    // the part that moves is the one it moves with.
    let mut listed_in: HashMap<u64, usize> = HashMap::new();
    for (i, list) in lists.iter().enumerate() {
        for &key in list {
            if listed_in.insert(key, i).is_some() || !objects.root.contains(&key) {
                return Err(unsupported(format!(
                    "Object {key} is listed twice, or is not a top-level object; OpenMaker does \
                     not move the parts around it."
                )));
            }
        }
    }
    for &root in &objects.root {
        let part = listed_in.get(&root).map(|&i| &stored[i]);
        let inside = part
            .zip(geometry.absolute_bounds(root))
            .is_some_and(|(part, b)| {
                b.top >= part.top && b.top < part.top + part.height.max(f64::MIN_POSITIVE)
            });
        if !inside {
            return Err(unsupported(format!(
                "Object {root} is not inside the part that lists it; OpenMaker does not move the \
                 parts around it."
            )));
        }
    }

    let mut slots: Vec<Slot> = stored
        .iter()
        .enumerate()
        .map(|(i, p)| Slot {
            from: Some(i),
            part_type: p.part_type,
            size: p.height,
        })
        .collect();
    let find = |part: u64, part_type: i32| {
        stored
            .iter()
            .position(|p| p.id == part && p.part_type == part_type)
            .ok_or_else(|| {
                Refusal::Invalid(format!(
                    "The layout has no {} with key {part}.",
                    part_type_name(part_type)
                ))
            })
    };
    let described = match change {
        PartChange::Add { part_type, size } => {
            check_size(*size)?;
            let (Some(rank_new), true) = (part_rank(*part_type), ADDABLE_PARTS.contains(part_type))
            else {
                return Err(unsupported(format!(
                    "OpenMaker does not add a {} part; sub-summaries need a break field.",
                    part_type_name(*part_type)
                )));
            };
            if stored.iter().any(|p| p.part_type == *part_type) {
                return Err(Refusal::Invalid(format!(
                    "The layout has a {} already; FileMaker has one on a layout.",
                    part_type_name(*part_type)
                )));
            }
            let at = slots
                .iter()
                .position(|s| part_rank(s.part_type).is_some_and(|r| r > rank_new))
                .unwrap_or(slots.len());
            slots.insert(
                at,
                Slot {
                    from: None,
                    part_type: *part_type,
                    size: *size,
                },
            );
            format!("a {} {size} points high", part_type_name(*part_type))
        }
        PartChange::Delete { part, part_type } => {
            // As for adding: sub-summaries, which a layout may have several
            // of, are not deleted by this version (a retry could not tell
            // which one went).
            if !ADDABLE_PARTS.contains(part_type) {
                return Err(unsupported(format!(
                    "OpenMaker does not delete a {} part.",
                    part_type_name(*part_type)
                )));
            }
            let at = find(*part, *part_type)?;
            if !lists[at].is_empty() {
                return Err(unsupported(format!(
                    "The {} holds {} object(s); OpenMaker deletes empty parts only.",
                    part_type_name(*part_type),
                    lists[at].len()
                )));
            }
            if stored.len() == 1 {
                return Err(Refusal::Invalid("A layout keeps at least one part.".into()));
            }
            slots.remove(at);
            format!("the {} (part {part})", part_type_name(*part_type))
        }
        PartChange::Resize {
            part,
            part_type,
            size,
        } => {
            check_size(*size)?;
            let at = find(*part, *part_type)?;
            let bottom = stored[at].top + size;
            for &key in &lists[at] {
                if geometry
                    .absolute_bounds(key)
                    .is_some_and(|b| b.bottom > bottom)
                {
                    return Err(Refusal::Invalid(format!(
                        "Object {key} reaches below where the {} would end ({bottom} points \
                         down); move it first.",
                        part_type_name(*part_type)
                    )));
                }
            }
            slots[at].size = *size;
            format!(
                "the {} (part {part}) {size} points high",
                part_type_name(*part_type)
            )
        }
    };

    // Where each part goes, and how far each stored part moves.
    let mut tops = Vec::with_capacity(slots.len());
    let mut moves: HashMap<u64, f64> = HashMap::new();
    let mut offset = 0.0;
    for slot in &slots {
        tops.push(offset);
        if let Some(i) = slot.from {
            moves.insert(stored[i].id, offset - stored[i].top);
        }
        offset += slot.size;
    }
    if offset > MAX_COORDINATE {
        return Err(Refusal::Invalid(format!(
            "The parts would reach past {MAX_COORDINATE} points."
        )));
    }

    // The objects of each part that moves move with it: their frames, and
    // their object index entries.
    let mut part_of: HashMap<u64, u64> = HashMap::new();
    for (part, list) in stored.iter().zip(&lists) {
        for &key in list {
            part_of.insert(key, part.id);
        }
    }
    let shift = |root: u64| -> Result<f64, Refusal> {
        let part = part_of.get(&root).ok_or_else(|| {
            unsupported(format!(
                "Object {root} is in no part's list; OpenMaker does not move the parts around \
                 it."
            ))
        })?;
        Ok(moves.get(part).copied().unwrap_or(0.0))
    };
    let mut store = stored_bytes.clone();
    let mut changes = Vec::new();
    for &root in &objects.root {
        let delta = shift(root)?;
        if delta == 0.0 {
            continue;
        }
        let object = objects
            .objects
            .iter()
            .find(|o| o.id == root)
            .ok_or_else(|| unsupported(format!("The layout has no object {root}.")))?;
        let bounds = object
            .bounds
            .as_ref()
            .ok_or_else(|| unsupported(format!("Object {root} has no frame.")))?;
        let frame = Rect {
            top: bounds.top + delta,
            bottom: bounds.bottom + delta,
            ..bounds.clone()
        };
        store = encode::with_object_bounds(&store, root, &frame).ok_or_else(|| {
            unsupported(format!(
                "OpenMaker cannot rewrite object {root}'s frame in the stored object store."
            ))
        })?;
        changes.push((
            format!("[4, 5, L, 07] object {root}"),
            format!("moved {delta} points down with its part"),
        ));
    }
    let mut next = member.clone();
    if store != stored_bytes {
        next.set_child(stored_value(&[OBJECTS], &store));
    }
    if let Some(index) = member.child(&[OBJECT_INDEX]) {
        let order = preorder(&objects)?;
        let root_of = |mut key: u64| {
            for _ in 0..64 {
                match objects
                    .objects
                    .iter()
                    .find(|o| o.id == key)
                    .and_then(|o| o.parent)
                {
                    Some(parent) => key = parent,
                    None => return key,
                }
            }
            key
        };
        let mismatch = || {
            unsupported(
                "The layout's object index does not match its objects; OpenMaker does not move \
                 its parts."
                    .into(),
            )
        };
        let moved = shifted_index(index, &|sequence, value| {
            let object = *order.get(usize::from(sequence)).ok_or_else(mismatch)?;
            // An entry that names its object names the one at its place.
            if let Some(named) = value.and_then(crate::layout_objects::indexed_object) {
                if named != object {
                    return Err(mismatch());
                }
            }
            shift(root_of(object))
        })?;
        if &moved != index {
            changes.push((
                "[4, 5, L, 0a]".to_owned(),
                "the entries of the objects that moved, at their new tops".to_owned(),
            ));
            next.set_child(moved);
        }
    }

    // The parts, keyed 1 to n from the top.
    let stored_parts = member
        .child(&[PARTS])
        .ok_or_else(|| unsupported("The layout has no parts.".into()))?;
    let part_keys: Vec<Vec<u8>> = stored.iter().map(|p| serial::encode(p.id)).collect();
    // Everything of the parts node but its parts and last key stays as
    // stored.
    let mut parts_node = stored_parts.clone();
    parts_node
        .children
        .retain(|c| c.segment != [LAST_PART] && !part_keys.contains(&c.segment));
    for (i, (slot, &top)) in slots.iter().zip(&tops).enumerate() {
        let key = i as u64 + 1;
        let segment = serial::encode(key);
        let node = match slot.from {
            Some(j) => {
                let mut node = part_node(member, stored[j].id)
                    .ok_or_else(|| unsupported(format!("Part {} is not stored.", stored[j].id)))?
                    .clone();
                node.segment = segment.clone();
                let def = node
                    .child(&[DEFINITION])
                    .and_then(|d| d.assembled())
                    .ok_or_else(|| {
                        unsupported(format!("Part {} has no definition.", stored[j].id))
                    })?
                    .into_owned();
                let mut def = Some(def);
                if top != stored[j].top {
                    def = def.and_then(|d| {
                        encode::with_double(&d, "PartDef", encode::PART_OFFSET_FIELD, top)
                    });
                }
                if slot.size != stored[j].height {
                    def = def.and_then(|d| {
                        encode::with_double(&d, "PartDef", encode::PART_SIZE_FIELD, slot.size)
                    });
                }
                let def = def.ok_or_else(|| {
                    unsupported(format!(
                        "Part {}'s definition holds its offset or size in a form OpenMaker does \
                         not change.",
                        stored[j].id
                    ))
                })?;
                node.set_child(stored_value(&[DEFINITION], &def));
                if stored[j].id != key || top != stored[j].top || slot.size != stored[j].height {
                    changes.push((
                        format!("[4, 5, L, 03, {}]", hex(&segment)),
                        format!(
                            "the {} (was part {}): offset {top}, {} points high",
                            part_type_name(slot.part_type),
                            stored[j].id,
                            slot.size
                        ),
                    ));
                }
                node
            }
            None => {
                let (style, alt) = part_styles(slot.part_type)
                    .ok_or_else(|| unsupported("No style for the new part's type.".into()))?;
                changes.push((
                    format!("[4, 5, L, 03, {}]", hex(&segment)),
                    format!(
                        "a new {}: offset {top}, {} points high, no objects",
                        part_type_name(slot.part_type),
                        slot.size
                    ),
                ));
                OwnedNode::list(
                    segment.clone(),
                    vec![
                        OwnedNode::leaf(
                            vec![DEFINITION],
                            encode::part_def(slot.part_type as u64, 0, top, slot.size, style, alt),
                        ),
                        OwnedNode::leaf(vec![PART_OBJECTS], 0u32.to_be_bytes().to_vec()),
                    ],
                )
            }
        };
        parts_node.set_child(node);
    }
    let last = slots.len() as u64;
    let mut counter = stored_parts
        .child(&[LAST_PART])
        .cloned()
        .unwrap_or_else(|| OwnedNode::leaf(vec![LAST_PART], Vec::new()));
    counter.value = Some(key_string(last));
    parts_node.set_child(counter);
    changes.push((
        "[4, 5, L, 03, 00]".to_owned(),
        format!("the last part key: {last}"),
    ));
    next.set_child(parts_node);
    changes.insert(0, ("[4, 5, L, 03]".to_owned(), described));
    Ok(crate::layout_objects::Planned {
        member: next,
        changes,
    })
}

/// The object index `index` with each entry, nested ones included, keyed
/// `delta(sequence)` points further down (its key's top), its value and
/// entries below it kept. Refused when an entry's key is not one the index
/// keeps (top, left, pre-order index) or a new top leaves the `u16` range.
/// How far down the entry at a pre-order index, holding a value, moves.
type IndexShift<'a> = dyn Fn(u16, Option<&[u8]>) -> Result<f64, Refusal> + 'a;

fn shifted_index(index: &OwnedNode, delta: &IndexShift<'_>) -> Result<OwnedNode, Refusal> {
    let mut out = OwnedNode {
        children: Vec::new(),
        ..index.clone()
    };
    for entry in &index.children {
        let [t0, t1, l0, l1, s0, s1] = entry.segment[..] else {
            return Err(Refusal::Unsupported(
                "The layout's object index has an entry OpenMaker does not read.".into(),
            ));
        };
        let top = f64::from(u16::from_be_bytes([t0, t1]))
            + delta(u16::from_be_bytes([s0, s1]), entry.value.as_deref())?;
        let top = (top.fract() == 0.0 && (0.0..=f64::from(u16::MAX)).contains(&top))
            .then_some(top as u16)
            .ok_or_else(|| {
                Refusal::Unsupported(
                    "An object would move outside what the layout's object index keys.".into(),
                )
            })?;
        let mut moved = shifted_index(entry, delta)?;
        moved.segment = [top.to_be_bytes(), [l0, l1], [s0, s1]].concat();
        if out.child(&moved.segment).is_some() {
            return Err(Refusal::Unsupported(
                "Two of the layout's object index entries would share a key.".into(),
            ));
        }
        out.set_child(moved);
    }
    Ok(out)
}

/// Whether `member`'s parts already show `change`: a part of the type with
/// the size added, no part of the type deleted, the part at the size. A
/// retry of a change whose answer was lost is not made twice.
pub fn already_parts(member: &OwnedNode, change: &PartChange) -> bool {
    let Ok(parts) = parts(member) else {
        return false;
    };
    match change {
        PartChange::Add { part_type, size } => {
            ADDABLE_PARTS.contains(part_type)
                && parts
                    .iter()
                    .any(|p| p.part_type == *part_type && p.height == *size)
        }
        PartChange::Delete { part_type, .. } => {
            ADDABLE_PARTS.contains(part_type) && !parts.iter().any(|p| p.part_type == *part_type)
        }
        PartChange::Resize {
            part,
            part_type,
            size,
        } => {
            part_rank(*part_type).is_some()
                && parts
                    .iter()
                    .any(|p| p.id == *part && p.part_type == *part_type && p.height == *size)
        }
    }
}
