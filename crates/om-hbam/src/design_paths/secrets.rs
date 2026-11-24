//! The Secret paths of a script or layout member: the credentials in a
//! step's data that [`STEP_SECRETS`] names for the step they belong to,
//! the user name and password in an ODBC profile, and every slot of step
//! data whose step OpenMaker cannot identify.
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
//! In step data a slot is a two-byte segment `[00 n]` holding a text `01`,
//! a number `03`, a calculation `05` and other parts (script-steps.md 4.2).
//! The credentials the specs name are calculations, so of a credential
//! slot the calculation `05` is Secret, and the text `01` when a slot has
//! one; its number `03` is another setting (Create PDF's slot 7 also holds
//! "allow screen reader", 12.2). When the step cannot be identified (no
//! step list, one that does not read, no descriptor naming the data key,
//! a step id the step catalog does not have), every slot of its data is
//! Secret whole: not knowing fails toward keeping a value out of anything
//! shown. Input is hostile: what does not decode finds nothing to trust.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use super::revision::Mark;
use super::rules::{is_odbc_profile, PROFILE, STEP_SECRETS};
use super::{Kind, Path};
use crate::serial;
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

/// The step ids the catalog names, and 0, a button with no action
/// (layout-model.md 5.3.3).
fn known_steps() -> &'static BTreeSet<u16> {
    static KNOWN: OnceLock<BTreeSet<u16>> = OnceLock::new();
    KNOWN.get_or_init(|| {
        let mut out: BTreeSet<u16> = STEPS
            .lines()
            .skip(1)
            .filter_map(|line| {
                let mut fields = line.splitn(3, ',');
                let id = fields.next()?.parse().ok()?;
                let name = fields.next()?;
                (!name.is_empty()).then_some(id)
            })
            .collect();
        out.insert(0);
        out
    })
}

/// A Secret path of a member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Secret {
    /// Relative to the member.
    pub path: Path,
    pub holds: &'static str,
    pub mark: Mark,
}

/// The step each data node belongs to: by data key, from what holds the
/// steps. `None` when that does not read.
type Steps = Option<BTreeMap<Vec<u8>, u16>>;

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
            let steps = button_steps(member).map(|steps| {
                steps
                    .into_iter()
                    .map(|(key, step)| (serial::encode(key), step))
                    .collect()
            });
            Some((nodes, steps))
        }
        _ => None,
    }
}

/// The step that the data at `at` (relative to the member) belongs to,
/// when it can be identified.
fn step_of(at: &[Vec<u8>], steps: &Steps) -> Option<u16> {
    // The data key: `K` of `[05, K]` in a script, `O` of `[05, O, 11]` on
    // a layout.
    let key = at.get(1)?;
    steps
        .as_ref()?
        .get(key)
        .copied()
        .filter(|id| known_steps().contains(id))
}

/// The Secret paths of `member`, relative to it.
pub(super) fn of(kind: Kind, member: &OwnedNode) -> Vec<Secret> {
    let mut out = Vec::new();
    let Some((nodes, steps)) = step_data(kind, member) else {
        return out;
    };
    for (at, data) in nodes {
        let step = step_of(&at, &steps);
        for child in &data.children {
            let mut slot = at.clone();
            slot.push(child.segment.clone());
            if child.segment == PROFILE {
                if let Some(mark) = profile_mark(child) {
                    out.push(Secret {
                        path: slot,
                        holds: "an ODBC connection profile's user name and password",
                        mark,
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
                    holds: "a slot of a step OpenMaker cannot identify",
                    mark: Mark::Whole,
                }),
                Some(step) => {
                    let Some(holds) = credential(step, &child.segment) else {
                        continue;
                    };
                    for part in [SLOT_TEXT, SLOT_FORMULA] {
                        if child.child(part).is_some() {
                            let mut path = slot.clone();
                            path.push(part.to_vec());
                            out.push(Secret {
                                path,
                                holds,
                                mark: Mark::Whole,
                            });
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

/// Whether the path `below` (relative to a member of `kind`) is Secret: in
/// or below a Secret path of [`of`], or, for a path the member does not
/// hold, where one would be.
pub(super) fn is_secret(kind: Kind, member: &OwnedNode, below: &[&[u8]]) -> bool {
    let within =
        |path: &Path| path.len() <= below.len() && path.iter().zip(below).all(|(a, b)| a == b);
    if of(kind, member).iter().any(|s| within(&s.path)) {
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
    match step_of(&at, &steps) {
        None => true,
        Some(step) => {
            credential(step, part).is_some()
                && below
                    .get(depth + 1)
                    .is_none_or(|p| *p == SLOT_TEXT || *p == SLOT_FORMULA)
        }
    }
}

/// How an ODBC profile `86` is marked: its user name and password strings,
/// or the whole profile when it does not read as the spec gives it.
fn profile_mark(node: &OwnedNode) -> Option<Mark> {
    let Some(value) = node.assembled() else {
        return Some(Mark::Whole);
    };
    if !is_odbc_profile(&value) {
        return None;
    }
    Some(odbc_credentials(&value).map_or(Mark::Whole, |ranges| Mark::Parts(ranges.to_vec())))
}

/// The byte ranges of an ODBC profile's user name and password: after the
/// type `ODBC` and a `u16`, the strings data source name, user name and
/// password, each a byte count then its bytes (script-steps.md 11.0,
/// 11.5), then a `u16` of flags.
fn odbc_credentials(value: &[u8]) -> Option<[std::ops::Range<usize>; 2]> {
    let mut at = 6;
    let mut string = || {
        let first = *value.get(at)?;
        let (header, len) = match first {
            0x00..=0x7f => (1, usize::from(first)),
            0x80..=0xbf => (
                2,
                usize::from(u16::from_be_bytes([first, *value.get(at + 1)?]) & 0x7fff),
            ),
            0xc2 => (
                3,
                usize::from(u16::from_be_bytes(
                    value.get(at + 1..at + 3)?.try_into().ok()?,
                )),
            ),
            0xc4 => (
                5,
                u32::from_be_bytes(value.get(at + 1..at + 5)?.try_into().ok()?) as usize,
            ),
            _ => return None,
        };
        let start = at;
        at = at.checked_add(header)?.checked_add(len)?;
        (at <= value.len()).then_some(start..at)
    };
    let _source = string()?;
    let user = string()?;
    let password = string()?;
    (value.len() >= password.end + 2).then_some([user, password])
}

/// Each step of a script's step list: its data key's segment and its step
/// id; `None` when the list does not read.
fn script_steps(member: &OwnedNode) -> Option<BTreeMap<Vec<u8>, u16>> {
    let list = member.child(&[0x04])?.assembled()?;
    if list.len() % 28 != 0 {
        return None;
    }
    let mut out = BTreeMap::new();
    for d in list.chunks_exact(28) {
        let len = usize::from(d[0]);
        let step = i16::from_be_bytes([d[20], d[21]]).unsigned_abs();
        let Some(key) = d.get(1..1 + len).filter(|_| (2..=5).contains(&len)) else {
            continue;
        };
        let Some((&seg_len, segment)) = key.split_first() else {
            continue;
        };
        if usize::from(seg_len) == segment.len() {
            out.insert(segment.to_vec(), step);
        }
    }
    Some(out)
}

/// Each button and button group of a layout's object store: its object
/// key and its step id; `None` when the store does not read.
fn button_steps(member: &OwnedNode) -> Option<Vec<(u64, u16)>> {
    let store = member.child(&[0x07])?.assembled()?;
    let mut out = Vec::new();
    let mut fields = Fields::new(&store);
    for (field, value) in fields.by_ref() {
        let (2, Wire::Bytes(object)) = (field, value) else {
            continue;
        };
        let mut key = None;
        let mut step = None;
        for (field, value) in Fields::new(object) {
            match (field, value) {
                (2, Wire::Varint(k)) => key = Some(k),
                // buttonObj (16): scriptStep is field 3; groupObj (12): 1.
                (16, Wire::Bytes(button)) => step = step.or(step_id(button, 3)),
                (12, Wire::Bytes(group)) => step = step.or(step_id(group, 1)),
                _ => {}
            }
        }
        if let (Some(key), Some(step)) = (key, step) {
            out.push((key, step));
        }
    }
    // A store that stops before its end does not read.
    (fields.at >= store.len()).then_some(out)
}

/// The id (field 1) of the `ScriptStep` at `field` of `message`.
fn step_id(message: &[u8], field: u32) -> Option<u16> {
    let (_, Wire::Bytes(step)) = Fields::new(message).find(|(f, _)| *f == field)? else {
        return None;
    };
    let (_, Wire::Varint(id)) = Fields::new(step).find(|(f, _)| *f == 1)? else {
        return None;
    };
    // An `int32`: a negative one is written as 64 bits.
    u16::try_from((id as i64 as i32).unsigned_abs()).ok()
}

/// A protobuf field's value.
#[derive(Clone, Copy)]
enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

/// The fields of a protobuf message, until the end or the first that does
/// not decode.
struct Fields<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Fields { buf, at: 0 }
    }

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
}

impl<'a> Iterator for Fields<'a> {
    type Item = (u32, Wire<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.buf.len() {
            return None;
        }
        let tag = self.varint()?;
        let field = u32::try_from(tag >> 3).ok()?;
        let value = match tag & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => {
                self.at = self
                    .at
                    .checked_add(8)
                    .filter(|&end| end <= self.buf.len())?;
                Wire::Fixed
            }
            2 => {
                let len = usize::try_from(self.varint()?).ok()?;
                let end = self
                    .at
                    .checked_add(len)
                    .filter(|&end| end <= self.buf.len())?;
                let bytes = &self.buf[self.at..end];
                self.at = end;
                Wire::Bytes(bytes)
            }
            5 => {
                self.at = self
                    .at
                    .checked_add(4)
                    .filter(|&end| end <= self.buf.len())?;
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

    #[test]
    fn button_steps_come_from_buttons_and_groups() {
        let step = |id: i64| {
            let mut s = Vec::new();
            varint(1 << 3, &mut s);
            varint(id as u64, &mut s);
            s
        };
        let object = |key: u64, field: u32, inner: u32, id: i64| {
            let mut holder = Vec::new();
            message(inner, &step(id), &mut holder);
            let mut o = Vec::new();
            varint(2 << 3, &mut o);
            varint(key, &mut o);
            message(field, &holder, &mut o);
            o
        };
        let mut store = Vec::new();
        message(2, &object(5, 16, 3, 138), &mut store);
        message(2, &object(200, 12, 1, -63), &mut store);
        let member = OwnedNode::list(vec![1], vec![OwnedNode::leaf(vec![7], store.clone())]);
        assert_eq!(button_steps(&member), Some(vec![(5, 138), (200, 63)]));
        // A cut store does not read.
        store.truncate(store.len() - 2);
        let member = OwnedNode::list(vec![1], vec![OwnedNode::leaf(vec![7], store)]);
        assert_eq!(button_steps(&member), None);
    }
}
