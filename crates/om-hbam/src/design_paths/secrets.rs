//! The Secret paths of a script or layout member: the credentials in a
//! step's data that [`STEP_SECRETS`] names for the step they belong to, and
//! everything in step data that is not identified with certainty.
//!
//! A script's step list `[11, 5, S, 04]` is its steps' 28-byte descriptors
//! (script-steps.md 2): the data key `mDataKey` in bytes 0-5 (a length,
//! then the key's segments each after its length), the step id `mStepID`
//! as a big-endian `i16` in bytes 20-21 (negative when disabled). The step
//! keeps its data at `[11, 5, S, 05, <data key>]` (4.2); `[11, 5, S, 05,
//! 00]` is the last issued data key, no step's data. A button keeps its
//! step in the layout's object store `[4, 5, L, 07]` (`LayoutObjectStore`,
//! `LayoutObject.buttonObj.scriptStep` or `groupObj.scriptStep`;
//! protobuf-schemas.md, `LayoutObject.proto`) and its data at
//! `[4, 5, L, 05, <object key as a serial key>, 11]` (layout-model.md
//! 5.3.3).
//!
//! **In a step's data, Design is only what an identified step's spec
//! names.** Each node below a data node is classed by its keys, its step
//! and, for a credential slot's other parts, its own shape ([`piece`]),
//! never by what any other node holds, so that no change to a credential
//! or to unidentified content can change how anything else is classed:
//!
//! - a slot `[00 n]` (script-steps.md 4.2) is Design with its list value,
//!   its text `01`, number `03`, calculation `05`, field spec `10`,
//!   occurrence keys `11` and path list `82`; any other part is not
//!   identified;
//! - of a slot [`STEP_SECRETS`] names as a credential slot, the text `01`
//!   and the calculation `05` are credentials, the slot's own value, which
//!   no spec gives one, is not identified, and a number `03`, field spec
//!   `10`, occurrence keys `11` or path list `82` is Design only in the
//!   shape 4.2 gives it (a number of 1 to 4 bytes, a field spec of 14), so
//!   that a credential written where none belongs is not taken for one;
//! - the step-level keys 4.2 names are Design, but for the profile `86`,
//!   which can hold a data source's user name and password (11.5) and is
//!   not read here, and the profile extension `87` of a step other than
//!   Print Setup and Print, which no spec describes; any other key, and a
//!   data node's own value, are not identified;
//! - all of a step's data is not identified when the step is not
//!   identified with certainty: no step list or object store, one that
//!   does not decode to its end, a descriptor whose data key does not
//!   decode or is `00`, two descriptors or objects naming one data key with
//!   different steps, a button whose step fields disagree (the layout
//!   decoder takes the last of repeated fields, `om_layout`), a step id the
//!   step catalog does not have (0, a button with no action, included), a
//!   data key no step names.
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
use super::rules::{PROFILE, STEP_SECRETS};
use super::{Kind, Path};
use crate::serial;
use crate::slot::be_uint;
use crate::tree::OwnedNode;

/// Step data's key in a script member.
const STEP_DATA: &[u8] = &[0x05];
/// The last issued data key in a script's step data (script-steps.md 4.2).
const LAST_DATA_KEY: &[u8] = &[0x00];
/// A layout object's step data in its attribute record.
const BUTTON_STEP: &[u8] = &[0x11];
/// A slot's text and calculation (script-steps.md 4.2): a credential
/// slot's credentials.
const SLOT_TEXT: &[u8] = &[0x01];
const SLOT_FORMULA: &[u8] = &[0x05];
/// The parts of a slot script-steps.md 4.2 names.
const SLOT_PARTS: &[u8] = &[0x01, 0x03, 0x05, 0x10, 0x11, 0x82];
/// The step-level keys script-steps.md 4.2 names, but the profile `86` and
/// its extension `87`.
const STEP_KEYS: &[u8] = &[
    0x06, 0x0a, 0x20, 0x82, 0x83, 0x84, 0x85, 0x88, 0x8c, 0xd7, 0xd8, 0xd9, 0xda, 0xf0,
];
/// `87`: Print Setup's and Print's page format (script-steps.md 11.6), and
/// for other steps a profile extension no spec describes.
const PAGE_FORMAT: &[u8] = &[0x87];
const PRINT_SETUP: u16 = 42;
const PRINT: u16 = 43;

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

/// What a Secret path covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Scope {
    /// The node and everything below it.
    Subtree,
    /// The node's own value only; what lies below is classed on its own.
    Own,
}

/// A Secret path of a member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Secret {
    /// Relative to the member.
    pub path: Path,
    pub holds: &'static str,
    /// An identified credential: the text or calculation of a credential
    /// slot of a step identified with certainty. Every other Secret path
    /// is Secret because what it holds could not be identified, and may
    /// hold design too.
    pub credential: bool,
    pub scope: Scope,
}

/// What a node in a step's data is, by its keys and its step alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Piece {
    Design,
    /// An identified credential, with everything below it.
    Credential(&'static str),
    /// Not identified, with everything below it.
    Unidentified(&'static str),
    /// The node's own value is not identified; what lies below is classed
    /// on its own.
    OwnUnidentified(&'static str),
}

/// What the node at `below` (relative to a data node, which is the empty
/// path) holds, `step` being the data node's step when it is identified
/// with certainty (module doc); `node` is the node, when the member holds
/// it.
pub(super) fn piece(step: Option<u16>, below: &[&[u8]], node: Option<&OwnedNode>) -> Piece {
    let Some((&first, rest)) = below.split_first() else {
        return Piece::OwnUnidentified("the own value of a step's data, which no spec names");
    };
    let Some(step) = step else {
        return Piece::Unidentified("data of a step OpenMaker cannot identify with certainty");
    };
    if first.len() == 2 {
        let holds = credential(step, first);
        return match (rest.first(), holds) {
            (None, Some(_)) => Piece::OwnUnidentified(
                "the own value of a credential slot, which no spec gives one",
            ),
            (None, None) => Piece::Design,
            (Some(&part), Some(holds)) if part == SLOT_TEXT || part == SLOT_FORMULA => {
                Piece::Credential(holds)
            }
            (Some(&part), _) if !(part.len() == 1 && SLOT_PARTS.contains(&part[0])) => {
                Piece::Unidentified("a part of a slot no spec names")
            }
            (Some(&part), Some(_))
                if rest.len() == 1 && node.is_some_and(|n| !part_shape(part, n)) =>
            {
                Piece::Unidentified("a part of a credential slot not in the shape the spec gives")
            }
            (Some(_), _) => Piece::Design,
        };
    }
    if first == PROFILE {
        return Piece::Unidentified(
            "a profile, which can hold a data source's user name and password",
        );
    }
    if first == PAGE_FORMAT {
        return match step {
            PRINT_SETUP | PRINT => Piece::Design,
            _ => Piece::Unidentified("a profile extension, which no spec describes"),
        };
    }
    if first.len() == 1 && STEP_KEYS.contains(&first[0]) {
        return Piece::Design;
    }
    Piece::Unidentified("a key of step data no spec names")
}

/// Whether a slot's number `03`, field spec `10`, occurrence keys `11` or
/// path list `82` has the shape script-steps.md 4.2 gives it.
fn part_shape(part: &[u8], node: &OwnedNode) -> bool {
    // A leaf holding bytes, or a split value (file-format.md 3.6).
    let bytes =
        |n: &OwnedNode| (n.children.is_empty() && n.value.is_some()) || split_value(n).is_some();
    let leaf_of = |range: std::ops::RangeInclusive<usize>| {
        node.children.is_empty()
            && node
                .value
                .as_ref()
                .is_some_and(|v| range.contains(&v.len()))
    };
    match part {
        // Numeric data: 1 to 4 bytes (`SetNumericData<T>`, 4.2, 11.7).
        [0x03] => leaf_of(1..=4),
        // A field spec: 14 bytes (`DBFieldSpec`).
        [0x10] => leaf_of(14..=14),
        // Occurrence keys, with a count `04` when several.
        [0x11] => {
            bytes(node)
                || (node.value.is_none() && node.children.iter().all(|k| k.children.is_empty()))
        }
        // A path list: text.
        [0x82] => bytes(node),
        _ => false,
    }
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
                .filter(|n| n.segment != LAST_DATA_KEY)
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
/// when it is identified with certainty.
fn resolve(at: &[Vec<u8>], steps: &Steps) -> Option<u16> {
    // The data key: `K` of `[05, K]` in a script, `O` of `[05, O, 11]` on
    // a layout.
    let key = at.get(1)?;
    let step = (*steps.as_ref()?.get(key)?)?;
    known_steps().contains(&step).then_some(step)
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
pub(super) fn misread(kind: Kind, member: &OwnedNode) -> bool {
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

fn unidentified(path: Path, holds: &'static str, scope: Scope) -> Secret {
    Secret {
        path,
        holds,
        credential: false,
        scope,
    }
}

/// The Secret paths of `member`, relative to it, in order: each node in
/// its step data that [`piece`] does not find Design, and every key of a
/// member the decoders could read differently.
pub(super) fn of(kind: Kind, member: &OwnedNode) -> Vec<Secret> {
    let mut out = Vec::new();
    if misread(kind, member) {
        return member
            .children
            .iter()
            .map(|c| {
                unidentified(
                    vec![c.segment.clone()],
                    "a member whose keys the decoders can read differently",
                    Scope::Subtree,
                )
            })
            .collect();
    }
    if kind == Kind::Script && member.child(STEP_DATA).is_some_and(|d| d.value.is_some()) {
        out.push(unidentified(
            vec![STEP_DATA.to_vec()],
            "the own value of a script's step data, which no spec names",
            Scope::Own,
        ));
    }
    let Some((nodes, steps)) = step_data(kind, member) else {
        return out;
    };
    for (at, data) in nodes {
        let step = resolve(&at, &steps);
        let mut put = |path: Path, piece: Piece, node: &OwnedNode| match piece {
            Piece::Design => {}
            Piece::Credential(holds) => out.push(Secret {
                path,
                holds,
                credential: true,
                scope: Scope::Subtree,
            }),
            Piece::Unidentified(holds) => out.push(unidentified(path, holds, Scope::Subtree)),
            Piece::OwnUnidentified(holds) => {
                if node.value.is_some() {
                    out.push(unidentified(path, holds, Scope::Own));
                }
            }
        };
        put(at.clone(), piece(step, &[], Some(data)), data);
        for child in &data.children {
            let mut path = at.clone();
            path.push(child.segment.clone());
            let own = piece(step, &[&child.segment], Some(child));
            put(path.clone(), own, child);
            if matches!(own, Piece::Unidentified(_)) || child.segment.len() != 2 {
                continue;
            }
            for part in &child.children {
                let mut part_path = path.clone();
                part_path.push(part.segment.clone());
                put(
                    part_path,
                    piece(step, &[&child.segment, &part.segment], Some(part)),
                    part,
                );
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.scope.cmp(&b.scope)));
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
/// or above a Secret path of [`of`] covering its subtree, at or above one
/// covering a node's own value, or, for a path the member does not hold,
/// where one would be.
pub(super) fn is_secret(kind: Kind, member: &OwnedNode, below: &[&[u8]]) -> bool {
    if misread(kind, member) {
        return true;
    }
    let secret = of(kind, member).into_iter().any(|s| {
        let prefix = s.path.iter().zip(below).all(|(a, b)| a == b);
        match s.scope {
            Scope::Subtree => prefix,
            Scope::Own => prefix && below.len() <= s.path.len(),
        }
    });
    if secret {
        return true;
    }
    // A path the member does not hold: decide by where it would be.
    let depth = match kind {
        Kind::Script
            if below.first() == Some(&STEP_DATA) && below.get(1) != Some(&LAST_DATA_KEY) =>
        {
            2
        }
        Kind::Layout
            if below.first() == Some(&&[0x05][..]) && below.get(2) == Some(&BUTTON_STEP) =>
        {
            3
        }
        _ => return false,
    };
    if below.len() <= depth || member.at(below).is_some() {
        return false;
    }
    let Some((_, steps)) = step_data(kind, member) else {
        return true;
    };
    let at: Path = below[..depth].iter().map(|s| s.to_vec()).collect();
    piece(resolve(&at, &steps), &below[depth..], None) != Piece::Design
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
        // `00` is the last issued data key, never a step's (4.2).
        if usize::from(seg_len) != segment.len()
            || serial::decode(segment).is_none()
            || segment == LAST_DATA_KEY
        {
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
