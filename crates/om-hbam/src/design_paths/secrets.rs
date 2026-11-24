//! The Secret paths of a script or layout member: the credentials in a
//! step's data that [`STEP_SECRETS`] names for the step they belong to,
//! ODBC profiles, and every slot of step data whose step OpenMaker cannot
//! identify with certainty.
//!
//! A script's step list `[11, 5, S, 04]` is its steps' 28-byte descriptors
//! (script-steps.md 2): the data key `mDataKey` in bytes 0-5 (a length,
//! then the key's segments each after its length), the step id `mStepID`
//! as a big-endian `i16` in bytes 20-21 (negative when disabled). The step
//! keeps its data at `[11, 5, S, 05, <data key>]` (4.2). A button keeps its
//! step in the layout's object store `[4, 5, L, 07]` (`LayoutObjectStore`,
//! `LayoutObject.buttonObj.scriptStep` or `groupObj.scriptStep`;
//! protobuf-schemas.md, `LayoutObject.proto`) and its data at
//! `[4, 5, L, 05, <object key as a serial key>, 11]` (layout-model.md
//! 5.3.3).
//!
//! In step data a slot is a two-byte segment `[00 n]` holding a list value
//! (a `u32`), a text `01`, a number `03`, a calculation `05`, a field spec
//! `10`, occurrence keys `11` and a path list `82` (script-steps.md 4.2).
//! The credentials the specs name are calculations, so of a credential
//! slot the calculation `05` is Secret, and the text `01` if it has one;
//! its number `03` is another setting (Create PDF's slot 7 also holds
//! "allow screen reader", 12.2). An ODBC profile `86` is Secret whole.
//!
//! "Not Secret" means identified and known not to hold a credential. So
//! every slot of a step's data is Secret, whole, when the step is not
//! identified with certainty: no step list or object store, one that does
//! not decode to its end, a descriptor whose data key does not decode, two
//! descriptors or objects naming one data key with different steps, a
//! button whose step fields disagree (the layout decoder takes the last of
//! repeated fields, `om_layout`), a step id the step catalog does not have
//! (0, a button with no action, included), a data key no step names; and
//! when a credential slot of the step does not have the shape above.
//!
//! The decoders find a script's step list `04` and step data `05`, a
//! layout's attribute records `05` and object store `07`, and a button's
//! step data `11` by number (`Node::get`, a key of 1 to 8 bytes read
//! big-endian: `[00 04]` reads as 4 and, sorting first, wins over `[04]`);
//! data nodes, slots and attribute records by exact key; and om-layout's
//! protobuf reader refuses field number 0 and numbers above 2^29 - 1. This
//! reading follows them or is stricter: a member, or a layout's attribute
//! record, with a key of more than one byte that reads as one of those
//! numbers is Secret whole (a web viewer's address `00 00`, number 0, is
//! not); a data key must be a serial key (4.2); a protobuf field this
//! reader does not take as om-layout does (a group, a field number out of
//! range, an id that is no `int32`) leaves the button unidentified.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use super::revision::split_value;
use super::rules::{is_odbc_profile, PROFILE, STEP_SECRETS};
use super::{Kind, Path};
use crate::serial;
use crate::slot::be_uint;
use crate::tree::OwnedNode;

/// Step data's key in a script member.
const STEP_DATA: &[u8] = &[0x05];
/// A layout object's step data in its attribute record.
const BUTTON_STEP: &[u8] = &[0x11];
/// A slot's text and calculation (script-steps.md 4.2).
const SLOT_TEXT: &[u8] = &[0x01];
const SLOT_FORMULA: &[u8] = &[0x05];

/// The step catalog (`docs/re/catalogs/script-steps.csv`).
const STEPS: &str = include_str!("../../../../docs/re/catalogs/script-steps.csv");

/// The step ids the catalog names.
fn known_steps() -> &'static BTreeSet<u16> {
    static KNOWN: OnceLock<BTreeSet<u16>> = OnceLock::new();
    KNOWN.get_or_init(|| {
        STEPS
            .lines()
            .skip(1)
            .filter_map(|line| {
                let mut fields = line.splitn(3, ',');
                let id = fields.next()?.parse().ok()?;
                let name = fields.next()?;
                (!name.is_empty()).then_some(id)
            })
            .collect()
    })
}

/// A Secret path of a member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Secret {
    /// Relative to the member.
    pub path: Path,
    pub holds: &'static str,
}

/// The step each data node belongs to, by data key: `None` inside where
/// two sources disagree. `None` outside when what holds the steps does not
/// decode.
type Steps = Option<BTreeMap<Vec<u8>, Option<u16>>>;

/// Step data in a member: each data node's path (relative to the member)
/// and node, and the steps they belong to.
fn step_data(kind: Kind, member: &OwnedNode) -> Option<(Vec<(Path, &OwnedNode)>, Steps)> {
    match kind {
        Kind::Script => {
            let data = member.child(STEP_DATA)?;
            let nodes = data
                .children
                .iter()
                .map(|n| (vec![STEP_DATA.to_vec(), n.segment.clone()], n))
                .collect();
            Some((nodes, script_steps(member)))
        }
        Kind::Layout => {
            let records = member.child(&[0x05])?;
            let nodes = records
                .children
                .iter()
                .filter_map(|r| {
                    let data = r.child(BUTTON_STEP)?;
                    Some((
                        vec![vec![0x05], r.segment.clone(), BUTTON_STEP.to_vec()],
                        data,
                    ))
                })
                .collect();
            Some((nodes, button_steps(member)))
        }
        _ => None,
    }
}

/// The step that the data at `at` (relative to the member) belongs to,
/// when it is identified with certainty and its credential slots have the
/// shape the spec gives.
fn resolve(at: &[Vec<u8>], data: Option<&OwnedNode>, steps: &Steps) -> Option<u16> {
    // The data key: `K` of `[05, K]` in a script, `O` of `[05, O, 11]` on
    // a layout.
    let key = at.get(1)?;
    let step = (*steps.as_ref()?.get(key)?)?;
    if !known_steps().contains(&step) {
        return None;
    }
    if let (Some(secret), Some(data)) = (STEP_SECRETS.iter().find(|s| s.step == step), data) {
        for (slot, _) in secret.slots {
            if data
                .child(&slot.to_be_bytes())
                .is_some_and(|s| !slot_shape(s))
            {
                return None;
            }
        }
    }
    Some(step)
}

/// Whether a slot has the shape script-steps.md 4.2 gives it.
fn slot_shape(slot: &OwnedNode) -> bool {
    // A leaf holding bytes, or a split value (file-format.md 3.6).
    let bytes =
        |n: &OwnedNode| (n.children.is_empty() && n.value.is_some()) || split_value(n).is_some();
    if slot.value.as_ref().is_some_and(|v| v.len() != 4) {
        return false;
    }
    slot.children.iter().all(|c| match c.segment.as_slice() {
        // Text, a path list: text values.
        [0x01] | [0x82] => bytes(c),
        // Numeric data: 1 to 4 bytes (`SetNumericData<T>`, 4.2, 11.7).
        [0x03] => {
            c.children.is_empty() && c.value.as_ref().is_some_and(|v| (1..=4).contains(&v.len()))
        }
        // A calculation: context, formatted and Polish streams (calc-encoding.md 1).
        [0x05] => {
            c.value.is_none()
                && c.children
                    .iter()
                    .all(|f| matches!(f.segment.as_slice(), [4] | [5] | [6]) && bytes(f))
        }
        // A field spec: 14 bytes (`DBFieldSpec`).
        [0x10] => c.children.is_empty() && c.value.as_ref().is_some_and(|v| v.len() == 14),
        // Occurrence keys, with a count `04` when several.
        [0x11] => {
            bytes(c) || (c.value.is_none() && c.children.iter().all(|k| k.children.is_empty()))
        }
        _ => false,
    })
}

/// Whether a key of `node` longer than one byte reads by number as one of
/// `numbers`, which the decoders look up in it by number.
fn aliasing(node: &OwnedNode, numbers: &[u8]) -> bool {
    node.children.iter().any(|c| {
        c.segment.len() > 1
            && be_uint(&c.segment).is_some_and(|n| numbers.iter().any(|&k| n == u64::from(k)))
    })
}

/// Whether the decoders could find another node than this reading does in
/// a member of `kind`: a key that reads as the step list or step data of a
/// script, the attribute records or object store of a layout, or a
/// button's step data in an attribute record.
fn misread(kind: Kind, member: &OwnedNode) -> bool {
    match kind {
        Kind::Script => aliasing(member, &[0x04, STEP_DATA[0]]),
        Kind::Layout => {
            aliasing(member, &[0x05, 0x07])
                || member.child(&[0x05]).is_some_and(|records| {
                    records.children.iter().any(|r| aliasing(r, BUTTON_STEP))
                })
        }
        _ => false,
    }
}

/// The Secret paths of `member`, relative to it.
pub(super) fn of(kind: Kind, member: &OwnedNode) -> Vec<Secret> {
    let mut out = Vec::new();
    if misread(kind, member) {
        return member
            .children
            .iter()
            .map(|c| Secret {
                path: vec![c.segment.clone()],
                holds: "a member whose keys the decoders can read differently",
            })
            .collect();
    }
    let Some((nodes, steps)) = step_data(kind, member) else {
        return out;
    };
    for (at, data) in nodes {
        let step = resolve(&at, Some(data), &steps);
        for child in &data.children {
            let mut slot = at.clone();
            slot.push(child.segment.clone());
            if child.segment == PROFILE {
                if profile_is_secret(child) {
                    out.push(Secret {
                        path: slot,
                        holds: "an ODBC connection profile, with its user name and password",
                    });
                }
                continue;
            }
            if child.segment.len() != 2 {
                continue;
            }
            match step {
                None => out.push(Secret {
                    path: slot,
                    holds: "a slot of a step OpenMaker cannot identify with certainty",
                }),
                Some(step) => {
                    let Some(holds) = credential(step, &child.segment) else {
                        continue;
                    };
                    for part in [SLOT_TEXT, SLOT_FORMULA] {
                        if child.child(part).is_some() {
                            let mut path = slot.clone();
                            path.push(part.to_vec());
                            out.push(Secret { path, holds });
                        }
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// What slot `segment` of step `step` holds, when it is a credential.
fn credential(step: u16, segment: &[u8]) -> Option<&'static str> {
    let slot = u16::from_be_bytes(segment.try_into().ok()?);
    let secret = STEP_SECRETS.iter().find(|s| s.step == step)?;
    secret
        .slots
        .iter()
        .find(|(n, _)| *n == slot)
        .map(|(_, holds)| *holds)
}

/// Whether a step's profile `86` is Secret: an ODBC connection profile, or
/// one that does not read.
fn profile_is_secret(node: &OwnedNode) -> bool {
    node.assembled().is_none_or(|value| is_odbc_profile(&value))
}

/// Whether the path `below` (relative to a member of `kind`) is Secret: in
/// or above a Secret path of [`of`], or, for a path the member does not
/// hold, where one would be.
pub(super) fn is_secret(kind: Kind, member: &OwnedNode, below: &[&[u8]]) -> bool {
    if misread(kind, member) {
        return true;
    }
    let related = |path: &Path| path.iter().zip(below).all(|(a, b)| a == b);
    if of(kind, member).iter().any(|s| related(&s.path)) {
        return true;
    }
    // A path the member does not hold: decide by where it would be.
    let depth = match kind {
        Kind::Script if below.first() == Some(&STEP_DATA) => 2,
        Kind::Layout
            if below.first() == Some(&&[0x05][..]) && below.get(2) == Some(&BUTTON_STEP) =>
        {
            3
        }
        _ => return false,
    };
    let Some(&part) = below.get(depth) else {
        return false;
    };
    if member.at(below).is_some() {
        return false;
    }
    if part == PROFILE {
        return true;
    }
    if part.len() != 2 {
        return false;
    }
    let Some((_, steps)) = step_data(kind, member) else {
        return true;
    };
    let at: Path = below[..depth].iter().map(|s| s.to_vec()).collect();
    let data = member.at(&below[..depth]);
    match resolve(&at, data, &steps) {
        None => true,
        Some(step) => {
            credential(step, part).is_some()
                && below
                    .get(depth + 1)
                    .is_none_or(|p| *p == SLOT_TEXT || *p == SLOT_FORMULA)
        }
    }
}

/// Each step of a script's step list, by its data key's segment; `None`
/// when the list or a descriptor's data key does not decode.
fn script_steps(member: &OwnedNode) -> Steps {
    let list = member.child(&[0x04])?.assembled()?;
    if list.len() % 28 != 0 {
        return None;
    }
    let mut out: BTreeMap<Vec<u8>, Option<u16>> = BTreeMap::new();
    for d in list.chunks_exact(28) {
        let len = usize::from(d[0]);
        if len == 0 {
            continue;
        }
        let key = d.get(1..1 + len).filter(|_| len <= 5)?;
        let (&seg_len, segment) = key.split_first()?;
        if usize::from(seg_len) != segment.len() || serial::decode(segment).is_none() {
            return None;
        }
        let step = i16::from_be_bytes([d[20], d[21]]).unsigned_abs();
        let entry = out.entry(segment.to_vec()).or_insert(Some(step));
        if *entry != Some(step) {
            *entry = None;
        }
    }
    Some(out)
}

/// Each button and button group of a layout's object store, by its object
/// key as a serial key; `None` when the store or an object in it does not
/// decode.
fn button_steps(member: &OwnedNode) -> Steps {
    let store = member.child(&[0x07])?.assembled()?;
    let mut out: BTreeMap<Vec<u8>, Option<u16>> = BTreeMap::new();
    for (field, value) in message(&store)? {
        let (2, Wire::Bytes(object)) = (field, value) else {
            continue;
        };
        let (key, step) = object_step(object)?;
        let Some(step) = step else { continue };
        let entry = out.entry(serial::encode(key)).or_insert(step);
        if *entry != step {
            *entry = None;
        }
    }
    Some(out)
}

/// A `LayoutObject`'s key and, when it is a button or a button group, its
/// step: `Some(None)` when its step fields are missing or disagree. `None`
/// when the object does not decode or has no single key.
fn object_step(object: &[u8]) -> Option<(u64, Option<Option<u16>>)> {
    let fields = message(object)?;
    let mut keys = fields.iter().filter(|(f, _)| *f == 2);
    let key = match (keys.next(), keys.next()) {
        (Some((_, Wire::Varint(key))), None) => *key,
        _ => return None,
    };
    let mut holders = 0;
    let mut ids = Vec::new();
    for (field, value) in &fields {
        // buttonObj (16): scriptStep is field 3; groupObj (12): 1.
        let step_field = match field {
            16 => 3,
            12 => 1,
            _ => continue,
        };
        holders += 1;
        let Wire::Bytes(holder) = value else {
            return None;
        };
        for (field, value) in message(holder)? {
            if field != step_field {
                continue;
            }
            let Wire::Bytes(step) = value else {
                return None;
            };
            for (field, value) in message(step)? {
                match (field, value) {
                    (1, Wire::Varint(id)) => ids.push(id),
                    (1, _) => return None,
                    _ => {}
                }
            }
        }
    }
    if holders == 0 {
        return Some((key, None));
    }
    let id = match ids.split_first() {
        Some((first, rest)) if rest.iter().all(|id| id == first) => int32_id(*first),
        _ => None,
    };
    Some((key, Some(id)))
}

/// A step id written as an `int32` (negative written as 64 bits), as a
/// step number; `None` when it is no `int32`.
fn int32_id(raw: u64) -> Option<u16> {
    let value = i32::try_from(raw as i64).ok()?;
    u16::try_from(value.unsigned_abs()).ok()
}

/// A protobuf field's value.
#[derive(Clone, Copy, Debug)]
enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

/// Every field of a protobuf message; `None` when it does not decode to
/// its end.
fn message(buf: &[u8]) -> Option<Vec<(u32, Wire<'_>)>> {
    let mut fields = Fields { buf, at: 0 };
    let mut out = Vec::new();
    while fields.at < buf.len() {
        out.push(fields.field()?);
    }
    Some(out)
}

/// A reader of a protobuf message's fields.
struct Fields<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = *self.buf.get(self.at)?;
            self.at += 1;
            value |= u64::from(byte & 0x7f).checked_shl(shift).unwrap_or(0);
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn skip(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self
            .at
            .checked_add(len)
            .filter(|&end| end <= self.buf.len())?;
        let bytes = &self.buf[self.at..end];
        self.at = end;
        Some(bytes)
    }

    fn field(&mut self) -> Option<(u32, Wire<'a>)> {
        let tag = self.varint()?;
        // om-layout's reader refuses field number 0 and numbers above
        // 2^29 - 1, the largest protobuf allows.
        let field = u32::try_from(tag >> 3)
            .ok()
            .filter(|f| (1..=(1 << 29) - 1).contains(f))?;
        let value = match tag & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => {
                self.skip(8)?;
                Wire::Fixed
            }
            2 => {
                let len = usize::try_from(self.varint()?).ok()?;
                Wire::Bytes(self.skip(len)?)
            }
            5 => {
                self.skip(4)?;
                Wire::Fixed
            }
            _ => return None,
        };
        Some((field, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    fn message(field: u32, bytes: &[u8], out: &mut Vec<u8>) {
        varint(u64::from(field) << 3 | 2, out);
        varint(bytes.len() as u64, out);
        out.extend_from_slice(bytes);
    }

    /// A `ScriptStep` holding the ids `ids` in its field 1.
    fn step(ids: &[i64]) -> Vec<u8> {
        let mut s = Vec::new();
        for &id in ids {
            varint(1 << 3, &mut s);
            varint(id as u64, &mut s);
        }
        s
    }

    /// A `LayoutObject` with key `key` and, in its field `field` (16 a
    /// button, 12 a group), a holder whose field `inner` is each of `steps`.
    fn object(key: u64, field: u32, inner: u32, steps: &[Vec<u8>]) -> Vec<u8> {
        let mut holder = Vec::new();
        for s in steps {
            message(inner, s, &mut holder);
        }
        let mut o = Vec::new();
        varint(2 << 3, &mut o);
        varint(key, &mut o);
        message(field, &holder, &mut o);
        o
    }

    fn steps_of(objects: &[Vec<u8>]) -> Steps {
        let mut store = Vec::new();
        for o in objects {
            message(2, o, &mut store);
        }
        button_steps(&OwnedNode::list(
            vec![1],
            vec![OwnedNode::leaf(vec![7], store)],
        ))
    }

    #[test]
    fn button_steps_come_from_buttons_and_groups_and_disagreement_is_no_step() {
        let found = |pairs: &[(u64, Option<u16>)]| {
            Some(
                pairs
                    .iter()
                    .map(|(k, s)| (serial::encode(*k), *s))
                    .collect::<BTreeMap<_, _>>(),
            )
        };
        assert_eq!(
            steps_of(&[
                object(5, 16, 3, &[step(&[138])]),
                object(200, 12, 1, &[step(&[-63])])
            ]),
            found(&[(5, Some(138)), (200, Some(63))])
        );
        // Repeated id fields, repeated step messages and two objects of
        // one key that disagree: no step. Repeated ones that agree: it.
        assert_eq!(
            steps_of(&[object(5, 16, 3, &[step(&[1, 138])])]),
            found(&[(5, None)])
        );
        assert_eq!(
            steps_of(&[object(5, 16, 3, &[step(&[1]), step(&[138])])]),
            found(&[(5, None)])
        );
        assert_eq!(
            steps_of(&[object(5, 16, 3, &[step(&[138, 138])])]),
            found(&[(5, Some(138))])
        );
        assert_eq!(
            steps_of(&[
                object(5, 16, 3, &[step(&[1])]),
                object(5, 12, 1, &[step(&[138])])
            ]),
            found(&[(5, None)])
        );
        // A button without a step, an id that is no int32.
        assert_eq!(steps_of(&[object(5, 16, 3, &[])]), found(&[(5, None)]));
        assert_eq!(
            steps_of(&[object(5, 16, 3, &[step(&[1 << 40])])]),
            found(&[(5, None)])
        );
        // A step message with a malformed suffix, an object without a key,
        // a cut store: nothing decodes.
        let mut torn = step(&[138]);
        torn.push(0x0a);
        assert_eq!(steps_of(&[object(5, 16, 3, &[torn])]), None);
        let mut keyless = Vec::new();
        message(16, &[], &mut keyless);
        assert_eq!(steps_of(&[keyless]), None);
        let mut store = Vec::new();
        message(2, &object(5, 16, 3, &[step(&[138])]), &mut store);
        store.truncate(store.len() - 2);
        let member = OwnedNode::list(vec![1], vec![OwnedNode::leaf(vec![7], store)]);
        assert_eq!(button_steps(&member), None);
    }
}
