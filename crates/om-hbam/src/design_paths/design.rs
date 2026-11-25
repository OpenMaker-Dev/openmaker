//! Design revisions, and the classes of an object's content that the
//! comparison of two files' designs ([`super::compare`]) compares
//! (`docs/server-storage.md` section 9; `docs/authoring.md`,
//! "Revisions").
//!
//! **What an object holds.** A member object is its member `[cat, 5, id]`.
//! A catalog object is everything in its catalog that is in no member
//! object: the catalog's own value, its index with every entry (its
//! members' entries too), any branch beside the index and the members, the
//! member list's own value, and the members that are no object (a member
//! with no children). Nothing moves between objects when a member appears
//! or is emptied, and nothing is held by two.
//!
//! **Pieces and classes.** An object's content is its pieces: each node's
//! being there and each node's own value. Each piece is one [`Class`],
//! decided by its own path alone, never by what lies below it:
//!
//! - **step data**, [`Class::StepData`]: a script's step data `[11, 5, S,
//!   05]` and a button's `[4, 5, L, 05, O, 11]`, each one opaque piece with
//!   everything below it. Nothing looks inside it: which of its bytes are a
//!   stored password depends on the step, and no reading of it is trusted
//!   here.
//! - Design, Data and Derived by the path rules ([`super::rules()`]); a
//!   node above the rules, such as the member itself, is there as Design
//!   (the object's shape) and its own value, which no rule names, is not
//!   identified.
//! - **not identified**, [`Class::Unidentified`]: a key no rule names, and
//!   everything below it.
//!
//! A split value (file-format.md 3.6) is one piece, its joined value, when
//! all of it is one class; otherwise each of its pieces is its own. Its
//! header's words after the length (a text's character count) are framing,
//! in no projection, as for the edit token. Modification stamps are left
//! out at exactly two places, where schema-path.md 8 places them: the
//! direct stamp keys of a member object's own node, and of a catalog's
//! index. Anywhere else a stamp-shaped key is a piece like any other.
//!
//! **Projections.** A class's projection of an object is built by walking
//! the object and adding the pieces of that class: the edit token's general
//! form ([`super::revision`]: segments, values and child counts, framed,
//! without list flags) over the nodes holding them and the nodes above
//! them, each node written with only its children that lead to such
//! pieces, so no value, length, count or being there of another class
//! reaches it. A difference only in whether a node is stored as a list is
//! no difference. The bytes start with eight `ff` bytes, which no edit
//! token's input can start with (they would be a segment length of 2^64 -
//! 1), then [`DESIGN_REVISION_VERSION`] and the class.
//!
//! **Design revisions.** An object's [`DesignRevision`] is the SHA-256 of
//! its Design projection, when the object holds no step data and no
//! unidentified piece; otherwise it has none. A design revision is
//! comparable only with one of the same version. The other projections are
//! hashed only to compare two objects inside the process and are never
//! given out. A member the decoders could read differently is not
//! classified at all ([`Refusal::Misread`]): where its step data is, is in
//! doubt.

use std::borrow::Cow;
use std::fmt;

use super::revision::{is_stamp, split_value};
use super::rules::{hex, Group, Rule};
use super::{candidates, secrets, step, Kind, Object};
use crate::tree::OwnedNode;

/// The version of the hashed form, written after the eight `ff` bytes. A
/// change to what a design revision covers or how it is written takes a
/// new version, and a new text form.
pub const DESIGN_REVISION_VERSION: u32 = 1;

/// How many segments a path may have before the object is refused. A
/// file's keys have 17 at most (file-format.md 2.2), so only a tree built
/// in memory reaches it; the walk recurses once per segment.
const MAX_DEPTH: usize = 64;

/// The SHA-256 of an object's Design projection (module doc). Made only
/// here, from Design content, so an edit token or a hash of other
/// content can never be one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DesignRevision([u8; 32]);

impl DesignRevision {
    /// Its text form: `design-sha256:` and 64 lowercase hex digits, which
    /// [`super::parse_revision`] refuses, so it cannot be passed where an
    /// edit token is expected. The text form of version 1.
    pub fn text(&self) -> String {
        format!("design-sha256:{}", hex(&self.0))
    }
}

impl fmt::Display for DesignRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

impl fmt::Debug for DesignRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

/// What a piece of an object's content is (module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    Design,
    Data,
    Derived,
    /// A script's or a button's step data, whole and unread.
    StepData,
    /// A key no rule names, and what lies below it.
    Unidentified,
}

impl Class {
    pub const ALL: [Class; 5] = [
        Class::Design,
        Class::Data,
        Class::Derived,
        Class::StepData,
        Class::Unidentified,
    ];

    fn bit(self) -> u8 {
        1 << self as u8
    }

    /// The class of a piece at a path whose path rules give `group`.
    fn of(group: Group) -> Class {
        match group {
            Group::Design => Class::Design,
            Group::Data => Class::Data,
            Group::Derived => Class::Derived,
            // No rule gives Access inside a design catalog, nor Secret.
            Group::Access | Group::Secret | Group::None => Class::Unidentified,
        }
    }
}

/// A node of an object, classified.
#[derive(Clone, Debug)]
pub(crate) struct Classed<'t> {
    pub segment: &'t [u8],
    /// The class of the node's being there.
    pub here: Class,
    /// Its own value (joined, for a split value written whole) and that
    /// value's class.
    pub value: Option<(Class, Cow<'t, [u8]>)>,
    /// The classes of the pieces at and below it.
    mask: u8,
    pub children: Vec<Classed<'t>>,
}

impl Classed<'_> {
    /// Whether a piece of `class` is at or below it.
    pub fn holds(&self, class: Class) -> bool {
        self.mask & class.bit() != 0
    }
}

/// An object, classified: its member, or a catalog's whole node.
#[derive(Clone, Debug)]
pub(crate) struct Classified<'t> {
    pub top: Classed<'t>,
    /// The key of the member's name.
    name_key: &'static [u8],
}

/// Why an object is not classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Deeper than [`MAX_DEPTH`].
    TooDeep,
    /// A member the decoders could read differently (`secrets::misread`).
    Misread,
}

/// What the walk is in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    /// A script's member.
    Script,
    /// A layout's member.
    Layout,
    /// Any other member.
    Member,
    /// A catalog's whole node, but its member objects.
    Catalog,
}

struct Walk {
    part: Part,
}

impl Walk {
    /// Whether the node at `rel` (relative to the part's top) is step data.
    fn step_data(&self, rel: &[&[u8]]) -> bool {
        matches!(
            (self.part, rel),
            (Part::Script, [[0x05]]) | (Part::Layout, [[0x05], _, [0x11]])
        )
    }

    /// Whether the child `child` of the node at `rel` is left out: a
    /// direct stamp of a member's own node or of a catalog's index, or a
    /// catalog's member that is an object of its own.
    fn left_out(&self, rel: &[&[u8]], child: &OwnedNode) -> bool {
        match (self.part, rel) {
            (Part::Catalog, [[0x01]]) => is_stamp(&child.segment),
            (Part::Catalog, [[0x05]]) => !child.children.is_empty(),
            (Part::Catalog, _) => false,
            (_, []) => is_stamp(&child.segment),
            _ => false,
        }
    }

    /// Classify `node` at `rel` (its path below the part's top), whose
    /// path has `depth` segments, with the path rules narrowed to it.
    fn node<'t>(
        &self,
        node: &'t OwnedNode,
        rel: &mut Vec<&'t [u8]>,
        depth: usize,
        (cands, best): (&[&'static Rule], Option<&'static Rule>),
        inside: bool,
    ) -> Result<Classed<'t>, Refusal> {
        if depth > MAX_DEPTH {
            return Err(Refusal::TooDeep);
        }
        let step_data = inside || self.step_data(rel);
        let (here, value_class) = if step_data {
            (Class::StepData, Class::StepData)
        } else {
            match best {
                Some(rule) => (Class::of(rule.group), Class::of(rule.group)),
                // Above the rules: the object's shape.
                None if !cands.is_empty() => (Class::Design, Class::Unidentified),
                None => (Class::Unidentified, Class::Unidentified),
            }
        };
        // A split value is its joined value when all of it is one class,
        // that of the node's being there.
        if let Some(joined) = split_value(node) {
            let pieces = self.children(node, rel, depth, (cands, best), step_data, false)?;
            let mask = pieces.iter().fold(here.bit(), |m, p| m | p.mask);
            if mask == here.bit() {
                return Ok(Classed {
                    segment: &node.segment,
                    here,
                    value: Some((here, Cow::Owned(joined))),
                    mask,
                    children: Vec::new(),
                });
            }
        }
        let children = self.children(node, rel, depth, (cands, best), step_data, true)?;
        let value = node
            .value
            .as_deref()
            .map(|v| (value_class, Cow::Borrowed(v)));
        let mask = children.iter().fold(
            here.bit() | value.as_ref().map_or(0, |(c, _)| c.bit()),
            |m, c| m | c.mask,
        );
        Ok(Classed {
            segment: &node.segment,
            here,
            value,
            mask,
            children,
        })
    }

    /// `node`'s children, classified; without what is left out when
    /// `filter`.
    fn children<'t>(
        &self,
        node: &'t OwnedNode,
        rel: &mut Vec<&'t [u8]>,
        depth: usize,
        (cands, best): (&[&'static Rule], Option<&'static Rule>),
        inside: bool,
        filter: bool,
    ) -> Result<Vec<Classed<'t>>, Refusal> {
        let mut out = Vec::with_capacity(node.children.len());
        for child in &node.children {
            if filter && self.left_out(rel, child) {
                continue;
            }
            let (deeper, rule) = step(cands, depth, &child.segment, best);
            rel.push(&child.segment);
            let classed = self.node(child, rel, depth + 1, (&deeper, rule), inside);
            rel.pop();
            out.push(classed?);
        }
        Ok(out)
    }
}

/// An object, classified.
pub(crate) fn classify<'t>(object: &Object<'t>) -> Result<Classified<'t>, Refusal> {
    let (part, path, node) = match (&object.whole, object.kind) {
        (Some((path, node)), _) => (Part::Catalog, path, *node),
        (None, Kind::Script) => (Part::Script, &object.path, object.node),
        (None, Kind::Layout) => (Part::Layout, &object.path, object.node),
        (None, _) => (Part::Member, &object.path, object.node),
    };
    if matches!(part, Part::Script | Part::Layout) && secrets::misread(object.kind, node) {
        return Err(Refusal::Misread);
    }
    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let (cands, best) = candidates(&segments);
    let walk = Walk { part };
    let top = walk.node(node, &mut Vec::new(), path.len(), (&cands, best), false)?;
    let name_key: &'static [u8] = match object.kind {
        Kind::Theme => &[0x0a],
        _ => &[0x10],
    };
    Ok(Classified { top, name_key })
}

fn framed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// The general form of `node` restricted to the pieces of `class`.
fn write(node: &Classed<'_>, class: Class, out: &mut Vec<u8>) {
    framed(out, node.segment);
    match &node.value {
        Some((own, value)) if *own == class => {
            out.push(1);
            framed(out, value);
        }
        _ => out.push(0),
    }
    let children: Vec<&Classed<'_>> = node.children.iter().filter(|c| c.holds(class)).collect();
    out.extend_from_slice(&(children.len() as u64).to_be_bytes());
    for child in children {
        write(child, class, out);
    }
}

impl Classified<'_> {
    /// Whether the member's name is one Design piece: there, with a value
    /// of its own (a leaf's, or a split value's written whole) and nothing
    /// below it.
    pub fn design_name(&self) -> bool {
        self.top.children.iter().any(|c| {
            c.segment == self.name_key
                && c.children.is_empty()
                && c.here == Class::Design
                && matches!(c.value, Some((Class::Design, _)))
        })
    }

    /// Whether the object holds a piece of `class`.
    pub fn holds(&self, class: Class) -> bool {
        self.top.holds(class)
    }

    /// The projection of `class` (module doc): the bytes hashed.
    pub fn projection(&self, class: Class) -> Vec<u8> {
        let mut out = vec![0xff; 8];
        out.extend_from_slice(&DESIGN_REVISION_VERSION.to_be_bytes());
        out.push(class as u8);
        write(&self.top, class, &mut out);
        out
    }

    /// The hash of each class's projection, by [`Class`] order: for
    /// comparing two objects in the process, never given out.
    pub fn digests(&self) -> [[u8; 32]; 5] {
        Class::ALL.map(|class| om_crypto::sha256(&self.projection(class)))
    }

    /// The design revision: `None` when the object holds step data or an
    /// unidentified piece.
    pub fn revision(&self) -> Option<DesignRevision> {
        (!self.holds(Class::StepData) && !self.holds(Class::Unidentified))
            .then(|| DesignRevision(om_crypto::sha256(&self.projection(Class::Design))))
    }
}

#[cfg(test)]
mod tests;
