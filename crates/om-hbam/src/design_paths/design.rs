//! Design revisions, and the classes of an object's content that the
//! comparison of two files' designs ([`super::compare`]) compares
//! (`docs/server-storage.md` section 9; `docs/authoring.md`,
//! "Revisions").
//!
//! **Pieces and classes.** An object's content is its pieces: each node
//! below the paths the rules name, being there, and each node's own value,
//! in its member (or index) and its index entries. Each piece is one
//! [`Class`], decided by its own path alone, never by what lies below it:
//! a node's being there is its path's class, with or without children (a
//! node above the rules, such as the member itself, is structure, no
//! piece). The class follows [`super::group_in`]: Design, Data and Derived
//! by the path rules; an identified credential (the text or calculation of
//! a credential slot of a step identified with certainty) is
//! [`Class::Credential`], with everything below it; everything else is
//! [`Class::Unidentified`]: a path in no group, and everything in step data
//! that is not identified (`secrets`' module doc: the data of a step not
//! identified with certainty, a profile, a key or part no spec names, the
//! own value of a data node or a credential slot). A split value
//! (file-format.md 3.6) is one piece, its joined value, when all of it is
//! one class; otherwise each of its pieces is its own. As for the edit
//! token, its header's words after the length (a text's character count)
//! are framing, in no projection. Modification stamps are left out where
//! schema-path.md 8 places them, on the member or index itself, and
//! nowhere else: a stamp-shaped key deeper is content like any other.
//!
//! **Projections.** A class's projection of an object is built by walking
//! the object and adding the pieces of that class: the canonical form of
//! the edit token ([`super::revision`]) over the nodes holding them and the
//! nodes above them, each node written with only its children that lead to
//! such pieces, so no value, length, count or being there of another class
//! reaches it. The member (or index) comes first, always with its key; then
//! the number of index entries holding pieces of the class, and each as its
//! index key and its node. A catalog's index leaves out the entries of its
//! members that are objects, and keeps any other. The bytes start with
//! eight `ff` bytes, which no edit token's input can start with (they would
//! be a segment length of 2^64 - 1), then [`DESIGN_REVISION_VERSION`] and
//! the class.
//!
//! **Design revisions.** An object's [`DesignRevision`] is the SHA-256 of
//! its Design projection, when the object holds no unidentified piece;
//! otherwise it has none. A design revision is comparable only with one of
//! the same version. The other projections are hashed only to compare two
//! objects inside the process and are never given out. A member the
//! decoders could read differently is not classified at all
//! ([`Refusal::Misread`]): which of its nodes are which is in doubt.

use std::borrow::Cow;
use std::fmt;

use super::revision::{is_stamp, split_value};
use super::rules::{hex, Group, Rule};
use super::{candidates, entry_keys, member_of, secrets, step, Catalog, Kind, Object, Path};
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
    /// An identified credential: Secret, and nothing but a credential.
    Credential,
    Data,
    Derived,
    /// A path in no group, or Secret because it could not be identified.
    Unidentified,
}

impl Class {
    pub const ALL: [Class; 5] = [
        Class::Design,
        Class::Credential,
        Class::Data,
        Class::Derived,
        Class::Unidentified,
    ];

    fn bit(self) -> u8 {
        1 << self as u8
    }

    /// The class of content at a path whose path rules give `group`.
    fn of(group: Group) -> Class {
        match group {
            Group::Design => Class::Design,
            Group::Data => Class::Data,
            Group::Derived => Class::Derived,
            // Access has no rule inside a design object; a Secret path is
            // found by the step rules, not the path rules.
            Group::Access | Group::Secret | Group::None => Class::Unidentified,
        }
    }

    /// Whether `group` ([`super::group_in`]'s answer for the same path)
    /// agrees with this class.
    #[cfg(test)]
    pub fn agrees_with(self, group: Group) -> bool {
        match self {
            Class::Design => group == Group::Design,
            Class::Credential => group == Group::Secret,
            Class::Data => group == Group::Data,
            Class::Derived => group == Group::Derived,
            Class::Unidentified => matches!(group, Group::None | Group::Secret | Group::Access),
        }
    }
}

/// The canonical form an object is written in: the edit token's for its
/// kind ([`super::edit_token`]), whose list flags the script form writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    Script,
    General,
}

/// A node of an object, classified.
#[derive(Clone, Debug)]
pub(crate) struct Classed<'t> {
    pub segment: &'t [u8],
    /// The list flag the script form writes: false for a split value
    /// written as its joined value.
    push: bool,
    /// The class of the node's being there: its path's (module doc); `None`
    /// for structure above the rules.
    pub here: Option<Class>,
    /// Its own value (joined, for a split value) and that value's class.
    pub value: Option<(Class, Cow<'t, [u8]>)>,
    /// The classes of the pieces at and below it.
    mask: u8,
    /// The nodes it and its subtree stand for that are a terminal or hold
    /// a value (each piece of a split value written whole counted).
    pub pieces: usize,
    pub children: Vec<Classed<'t>>,
}

impl Classed<'_> {
    /// Whether a piece of `class` is at or below it.
    pub fn holds(&self, class: Class) -> bool {
        self.mask & class.bit() != 0
    }
}

/// An object, classified: its member (or index) and its index entries,
/// each with its index key.
#[derive(Clone, Debug)]
pub(crate) struct Classified<'t> {
    form: Form,
    pub top: Classed<'t>,
    pub entries: Vec<(Vec<u8>, Classed<'t>)>,
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

/// What a member's Secret path marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Mark {
    /// The node and everything below it; an identified credential or not.
    Subtree(bool),
    /// The node's own value, not identified.
    Own,
}

/// Where a node lies relative to the member's Secret paths.
#[derive(Clone, Copy)]
enum At {
    /// No Secret path at or below it.
    Rules,
    /// At or below a Secret path covering its subtree: all of it is this
    /// class.
    Fixed(Class),
    /// Above the Secret paths in this range of the list.
    Above(usize, usize),
}

struct Walk<'s> {
    /// The member's Secret paths, relative to it, in order.
    secrets: &'s [(Path, Mark)],
    /// How many segments the member's path has: where the Secret paths
    /// start.
    base: usize,
    /// A catalog's entries that belong to its members' objects.
    entries: Option<Theirs<'s>>,
}

/// A catalog's index keys of entries, and its members that are objects,
/// whose entries in those keys are theirs.
#[derive(Clone, Copy)]
struct Theirs<'s> {
    keys: &'s [&'static [u8]],
    members: &'s [Vec<u8>],
}

impl Walk<'_> {
    /// Classify `node`, whose path has `depth` segments, `level` below the
    /// part's top, with the path rules narrowed to its path; `own` when its
    /// own value is marked not identified.
    fn node<'t>(
        &self,
        node: &'t OwnedNode,
        depth: usize,
        rules: (&[&'static Rule], Option<&'static Rule>),
        (at, own): (At, bool),
        level: usize,
    ) -> Result<Classed<'t>, Refusal> {
        if depth > MAX_DEPTH {
            return Err(Refusal::TooDeep);
        }
        let (cands, best) = rules;
        let structural = !matches!(at, At::Fixed(_)) && best.is_none() && !cands.is_empty();
        let path_class = match at {
            At::Fixed(class) => class,
            _ => best.map_or(Class::Unidentified, |r| Class::of(r.group)),
        };
        let here = (!structural).then_some(path_class);
        let value_class = if own || structural {
            Class::Unidentified
        } else {
            path_class
        };
        // A split value is its joined value when all of it is one class,
        // as the edit token writes it whole.
        if let Some(joined) = split_value(node) {
            let pieces = self.children(node, depth, rules, at, level, false)?;
            let mask = pieces.iter().fold(0, |m, p| m | p.mask);
            let class = Class::ALL.into_iter().find(|c| c.bit() == mask);
            if let Some(class) = class.filter(|c| !own && here.is_none_or(|h| h == *c)) {
                return Ok(Classed {
                    segment: &node.segment,
                    push: false,
                    here,
                    value: Some((class, Cow::Owned(joined))),
                    mask,
                    pieces: pieces.iter().map(|p| p.pieces).sum(),
                    children: Vec::new(),
                });
            }
        }
        let children = self.children(node, depth, rules, at, level, true)?;
        let value = node
            .value
            .as_deref()
            .map(|v| (value_class, Cow::Borrowed(v)));
        let mask = children.iter().fold(
            here.map_or(0, Class::bit) | value.as_ref().map_or(0, |(c, _)| c.bit()),
            |m, c| m | c.mask,
        );
        let own_piece = usize::from(node.value.is_some() || node.children.is_empty());
        Ok(Classed {
            segment: &node.segment,
            push: node.push,
            here,
            value,
            mask,
            pieces: own_piece + children.iter().map(|c| c.pieces).sum::<usize>(),
            children,
        })
    }

    /// `node`'s children, classified; when `filter`, without the top
    /// node's stamps and a catalog's members' entries.
    fn children<'t>(
        &self,
        node: &'t OwnedNode,
        depth: usize,
        (cands, best): (&[&'static Rule], Option<&'static Rule>),
        at: At,
        level: usize,
        filter: bool,
    ) -> Result<Vec<Classed<'t>>, Refusal> {
        let theirs = |child: &OwnedNode| {
            level == 1
                && self.entries.is_some_and(|theirs| {
                    theirs.keys.contains(&node.segment.as_slice())
                        && theirs.members.contains(&child.segment)
                })
        };
        let mut out = Vec::with_capacity(node.children.len());
        for child in &node.children {
            if filter && ((level == 0 && is_stamp(&child.segment)) || theirs(child)) {
                continue;
            }
            let (deeper, rule) = step(cands, depth, &child.segment, best);
            let child_at = self.below(at, depth, &child.segment);
            out.push(self.node(child, depth + 1, (&deeper, rule), child_at, level + 1)?);
        }
        Ok(out)
    }

    /// Where the child `segment` of a node whose path has `depth` segments
    /// lies, the node being at `at`, and whether its own value is marked.
    /// The Secret paths in a range all share the node's path below the
    /// member, so they are in order of their next segment, the shortest
    /// first.
    fn below(&self, at: At, depth: usize, segment: &[u8]) -> (At, bool) {
        let At::Above(lo, hi) = at else {
            return (at, false);
        };
        let i = depth - self.base;
        let range = &self.secrets[lo..hi];
        let start = lo + range.partition_point(|(p, _)| p[i].as_slice() < segment);
        let end = lo + range.partition_point(|(p, _)| p[i].as_slice() <= segment);
        let exact = self.secrets[start..end]
            .iter()
            .take_while(|(p, _)| p.len() == i + 1)
            .count();
        let mut own = false;
        let mut subtree: Option<bool> = None;
        for (_, mark) in &self.secrets[start..start + exact] {
            match mark {
                Mark::Own => own = true,
                Mark::Subtree(credential) => {
                    subtree = Some(subtree.unwrap_or(true) && *credential);
                }
            }
        }
        if let Some(credential) = subtree {
            let class = if credential {
                Class::Credential
            } else {
                Class::Unidentified
            };
            return (At::Fixed(class), false);
        }
        let deeper = start + exact;
        let at = if deeper == end {
            At::Rules
        } else {
            At::Above(deeper, end)
        };
        (at, own)
    }

    /// Classify the node at `path`, the top of a part of an object.
    fn part<'t>(
        &self,
        path: &[Vec<u8>],
        node: &'t OwnedNode,
        at: At,
    ) -> Result<Classed<'t>, Refusal> {
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let (cands, best) = candidates(&segments);
        self.node(node, path.len(), (&cands, best), (at, false), 0)
    }
}

/// An object, classified.
pub(crate) fn classify<'t>(object: &Object<'t>) -> Result<Classified<'t>, Refusal> {
    let form = match object.kind {
        Kind::Script if !object.catalog => Form::Script,
        _ => Form::General,
    };
    let segments: Vec<&[u8]> = object.path.iter().map(Vec::as_slice).collect();
    let mut marks: Vec<(Path, Mark)> = Vec::new();
    if !object.catalog {
        if let Some((kind, depth)) = member_of(&segments) {
            if depth == segments.len() {
                if secrets::misread(kind, object.node) {
                    return Err(Refusal::Misread);
                }
                marks = secrets::of(kind, object.node)
                    .into_iter()
                    .map(|s| {
                        let mark = match s.scope {
                            secrets::Scope::Subtree => Mark::Subtree(s.credential),
                            secrets::Scope::Own => Mark::Own,
                        };
                        (s.path, mark)
                    })
                    .collect();
                marks.sort();
            }
        }
    }
    let keys = if object.catalog {
        entry_keys(Catalog::of(object.kind))
    } else {
        Vec::new()
    };
    let walk = Walk {
        secrets: &marks,
        base: segments.len(),
        entries: object.catalog.then_some(Theirs {
            keys: &keys,
            members: &object.members,
        }),
    };
    let at = if marks.is_empty() {
        At::Rules
    } else {
        At::Above(0, marks.len())
    };
    let top = walk.part(&object.path, object.node, at)?;
    let entry_walk = Walk {
        secrets: &[],
        base: 0,
        entries: None,
    };
    let mut entries = Vec::with_capacity(object.entries.len());
    for (path, node) in &object.entries {
        // `[cat, 1, key, member]`: the index key.
        let key = path
            .len()
            .checked_sub(2)
            .map_or(Vec::new(), |i| path[i].clone());
        entries.push((key, entry_walk.part(path, node, At::Rules)?));
    }
    let name_key: &'static [u8] = match object.kind {
        Kind::Theme => &[0x0a],
        _ => &[0x10],
    };
    Ok(Classified {
        form,
        top,
        entries,
        name_key,
    })
}

fn framed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// The canonical form of `node` restricted to the pieces of `class`.
fn write(node: &Classed<'_>, class: Class, form: Form, out: &mut Vec<u8>) {
    framed(out, node.segment);
    if form == Form::Script {
        out.push(u8::from(node.push));
    }
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
        write(child, class, form, out);
    }
}

impl Classified<'_> {
    /// Whether the member's name is a Design piece: there, and its value,
    /// a leaf's or a split value's written whole.
    pub fn design_name(&self) -> bool {
        self.top.children.iter().any(|c| {
            c.segment == self.name_key
                && c.children.is_empty()
                && c.here == Some(Class::Design)
                && matches!(c.value, Some((Class::Design, _)))
        })
    }

    /// The pieces the object classified ([`Classed::pieces`]).
    #[cfg(test)]
    pub fn pieces(&self) -> usize {
        self.top.pieces + self.entries.iter().map(|(_, e)| e.pieces).sum::<usize>()
    }

    /// Whether the object holds a piece of `class`.
    pub fn holds(&self, class: Class) -> bool {
        self.top.holds(class) || self.entries.iter().any(|(_, e)| e.holds(class))
    }

    /// The projection of `class` (module doc): the bytes hashed.
    pub fn projection(&self, class: Class) -> Vec<u8> {
        let mut out = vec![0xff; 8];
        out.extend_from_slice(&DESIGN_REVISION_VERSION.to_be_bytes());
        out.push(class as u8);
        write(&self.top, class, self.form, &mut out);
        let entries: Vec<&(Vec<u8>, Classed<'_>)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.holds(class))
            .collect();
        out.extend_from_slice(&(entries.len() as u64).to_be_bytes());
        for (key, entry) in entries {
            framed(&mut out, key);
            write(entry, class, self.form, &mut out);
        }
        out
    }

    /// The hash of each class's projection, by [`Class`] order: for
    /// comparing two objects in the process, never given out.
    pub fn digests(&self) -> [[u8; 32]; 5] {
        Class::ALL.map(|class| om_crypto::sha256(&self.projection(class)))
    }

    /// The design revision: `None` when the object holds an unidentified
    /// piece.
    pub fn revision(&self) -> Option<DesignRevision> {
        (!self.holds(Class::Unidentified))
            .then(|| DesignRevision(om_crypto::sha256(&self.projection(Class::Design))))
    }
}

#[cfg(test)]
mod tests;
