//! The Secret paths of a script or layout member: the slots of step data
//! that [`STEP_SECRETS`] names for the step they belong to, and ODBC
//! profiles ([`is_odbc_profile`]).
//!
//! A script's step list `[17, 5, S, 04]` is its steps' 28-byte descriptors
//! (script-steps.md 2): the data key `mDataKey` in bytes 0-5 (a length,
//! then the key's segments each after its length), the step id `mStepID`
//! as a big-endian `i16` in bytes 20-21 (negative when disabled). The step
//! keeps its data at `[17, 5, S, 05, <data key>]` (4.2). A button keeps its
//! step in the layout's object store `[4, 5, L, 07]` (`LayoutObjectStore`,
//! `LayoutObject.buttonObj.scriptStep` or `groupObj.scriptStep`;
//! protobuf-schemas.md, `LayoutObject.proto`) and its data at
//! `[4, 5, L, 05, <object key as a serial key>, 11]` (layout-model.md
//! 5.3.3). Input is hostile: what does not decode finds nothing.

use super::rules::{is_odbc_profile, PROFILE, STEP_SECRETS};
use super::{Kind, Path};
use crate::serial;
use crate::tree::OwnedNode;

/// Step data's key in a script member.
const STEP_DATA: &[u8] = &[0x05];
/// A layout object's step data in its attribute record.
const BUTTON_STEP: &[u8] = &[0x11];

/// The Secret paths of `member`, relative to it, with what each holds.
pub(super) fn of(kind: Kind, member: &OwnedNode) -> Vec<(Path, &'static str)> {
    let mut out = Vec::new();
    match kind {
        Kind::Script => {
            let Some(data) = member.child(STEP_DATA) else {
                return out;
            };
            for (key, step) in script_steps(member) {
                if let Some(node) = data.child(&key) {
                    slots(step, node, &[STEP_DATA.to_vec(), key.clone()], &mut out);
                }
            }
            for node in &data.children {
                profile(node, &[STEP_DATA.to_vec(), node.segment.clone()], &mut out);
            }
        }
        Kind::Layout => {
            let Some(records) = member.child(&[0x05]) else {
                return out;
            };
            for (key, step) in button_steps(member) {
                let record = serial::encode(key);
                if let Some(node) = records.child(&record).and_then(|r| r.child(BUTTON_STEP)) {
                    let at = [vec![0x05], record.clone(), BUTTON_STEP.to_vec()];
                    slots(step, node, &at, &mut out);
                }
            }
            for record in &records.children {
                if let Some(node) = record.child(BUTTON_STEP) {
                    let at = [vec![0x05], record.segment.clone(), BUTTON_STEP.to_vec()];
                    profile(node, &at, &mut out);
                }
            }
        }
        _ => {}
    }
    out.sort();
    out.dedup();
    out
}

/// The slots of `data`, step `step`'s data at `at`, that hold credentials.
fn slots(step: u16, data: &OwnedNode, at: &[Vec<u8>], out: &mut Vec<(Path, &'static str)>) {
    let Some(secret) = STEP_SECRETS.iter().find(|s| s.step == step) else {
        return;
    };
    for (slot, holds) in secret.slots {
        let segment = slot.to_be_bytes();
        if data.child(&segment).is_some() {
            let mut path = at.to_vec();
            path.push(segment.to_vec());
            out.push((path, holds));
        }
    }
}

/// Step data's ODBC profile, which holds a password.
fn profile(data: &OwnedNode, at: &[Vec<u8>], out: &mut Vec<(Path, &'static str)>) {
    let Some(node) = data.child(PROFILE) else {
        return;
    };
    if node
        .assembled()
        .is_some_and(|value| is_odbc_profile(&value))
    {
        let mut path = at.to_vec();
        path.push(PROFILE.to_vec());
        out.push((path, "an ODBC connection profile with its password"));
    }
}

/// Each step of a script's step list with data: its data key's segment and
/// its step id.
fn script_steps(member: &OwnedNode) -> Vec<(Vec<u8>, u16)> {
    let Some(list) = member.child(&[0x04]).and_then(OwnedNode::assembled) else {
        return Vec::new();
    };
    list.chunks_exact(28)
        .filter_map(|d| {
            let len = usize::from(d[0]);
            let key = d.get(1..1 + len).filter(|_| (2..=5).contains(&len))?;
            let (&seg_len, segment) = key.split_first()?;
            if usize::from(seg_len) != segment.len() {
                return None;
            }
            let step = i16::from_be_bytes([d[20], d[21]]).unsigned_abs();
            Some((segment.to_vec(), step))
        })
        .collect()
}

/// Each button and button group of a layout's object store: its object
/// key and its step id.
fn button_steps(member: &OwnedNode) -> Vec<(u64, u16)> {
    let Some(store) = member.child(&[0x07]).and_then(OwnedNode::assembled) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (field, value) in Fields::new(&store) {
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
    out
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
        assert_eq!(button_steps(&member), vec![(5, 138), (200, 63)]);
        // A cut store finds what came before the cut.
        store.truncate(store.len() - 2);
        let member = OwnedNode::list(vec![1], vec![OwnedNode::leaf(vec![7], store)]);
        assert_eq!(button_steps(&member), vec![(5, 138)]);
    }
}
