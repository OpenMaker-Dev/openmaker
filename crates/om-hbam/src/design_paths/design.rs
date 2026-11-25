//! Design revisions, and the classes of an object's content that the
//! comparison of two files' designs ([`super::compare`]) compares
//! (`docs/server-storage.md` section 9; `docs/authoring.md`,
//! "Revisions").
//!
//! **Classes.** Each piece of an object's content is one [`Class`]: a
//! node's value, or an empty list's being there, in its member (or index)
//! and its index entries. The class follows [`super::group_in`]: Design,
//! Data and Derived as there; an identified credential (the text or
//! calculation of a credential slot of a step identified with certainty)
//! is [`Class::Credential`]; everything else is [`Class::Unidentified`]: a
//! path in no group, and a Secret path that is Secret because what it holds
//! could not be identified (every slot of a step not identified with
//! certainty, an ODBC profile, a member the decoders could read
//! differently, and the own value of a node above a Secret path, such as a
//! credential slot's list value), which may hold design as well. A split
//! value (file-format.md 3.6) is one piece of content, its joined value,
//! when all of it is one class; otherwise each of its pieces is its own.
//! As for the edit token, its header's words after the length (a text's
//! character count) are framing, in no projection.
//! Modification stamps are left out as the edit token leaves them out
//! (the script form only the top node's own), so they are no content.
//!
//! **Projections.** A class's projection of an object is built by walking
//! the object and adding what is in the class: the canonical form of the
//! edit token ([`super::revision`]) over the nodes holding that class's
//! content and the nodes above them, each node written with only its
//! children that lead to such content, so no value, length or count of
//! another class reaches it. The member (or index) comes first, always
//! with its key; then the number of index entries holding content of the
//! class, and each as its index key and its node. The bytes start with
//! eight `ff` bytes, which no edit token's input can start with (they
//! would be a segment length of 2^64 - 1), then [`DESIGN_REVISION_VERSION`]
//! and the class.
//!
//! **Design revisions.** An object's [`DesignRevision`] is the SHA-256 of
//! its Design projection, when the object holds no unidentified content;
//! otherwise it has none. A design revision is comparable only with one of
//! the same version. The other projections are hashed only to compare two
//! objects inside the process and are never given out.

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
/// kind ([`super::edit_token`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// A script's member and entries: list flags written, the top node's
    /// own stamps left out.
    Script,
    /// Everything else: no list flags, every stamp left out.
    General,
}

/// A node of an object, classified.
#[derive(Clone, Debug)]
pub(crate) struct Classed<'t> {
    pub segment: &'t [u8],
    /// The list flag the script form writes: false for a split value
    /// written as its joined value.
    push: bool,
    /// The node's own content and its class: its value (joined, for a split
    /// value), or `None` for an empty list's being there. `None` for a node
    /// that only holds others.
    pub own: Option<(Class, Option<Cow<'t, [u8]>>)>,
    /// The classes of the content at and below it.
    mask: u8,
    pub children: Vec<Classed<'t>>,
}

impl Classed<'_> {
    /// Whether content of `class` is at or below it.
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
    pub entries: Vec<(&'t [u8], Classed<'t>)>,
}

/// The object was deeper than [`MAX_DEPTH`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TooDeep;

/// Where a node lies relative to the member's Secret paths.
#[derive(Clone, Copy)]
enum At {
    /// No Secret path at, above or below it.
    Rules,
    /// At or below a Secret path: all of it is this class.
    Fixed(Class),
    /// Above the Secret paths in this range of the list.
    Above(usize, usize),
}

struct Walk<'s> {
    form: Form,
    /// The member's Secret paths, relative to it, in order, and whether
    /// each is an identified credential.
    secrets: &'s [(Path, bool)],
    /// How many segments the member's path has: where the Secret paths
    /// start.
    base: usize,
    /// Index keys left out of the top node: a catalog's entries, which
    /// belong to its members' objects.
    skip: &'s [&'static [u8]],
}

impl Walk<'_> {
    /// Classify `node`, whose path has `depth` segments, with the path
    /// rules `cands` and `best` narrowed to that path.
    fn node<'t>(
        &self,
        node: &'t OwnedNode,
        depth: usize,
        rules: (&[&'static Rule], Option<&'static Rule>),
        at: At,
        top: bool,
    ) -> Result<Classed<'t>, TooDeep> {
        if depth > MAX_DEPTH {
            return Err(TooDeep);
        }
        let own_class = match at {
            At::Fixed(class) => class,
            At::Above(..) => Class::Unidentified,
            At::Rules => rules.1.map_or(Class::Unidentified, |r| Class::of(r.group)),
        };
        // A split value is its joined value when all of it is one class,
        // as the edit token writes it whole.
        if let Some(joined) = split_value(node) {
            let pieces = self.children(node, depth, rules, at, top, false)?;
            let mask = pieces.iter().fold(0, |m, p| m | p.mask);
            if let Some(class) = Class::ALL.into_iter().find(|c| c.bit() == mask) {
                return Ok(Classed {
                    segment: &node.segment,
                    push: false,
                    own: Some((class, Some(Cow::Owned(joined)))),
                    mask,
                    children: Vec::new(),
                });
            }
        }
        let children = self.children(node, depth, rules, at, top, true)?;
        let own = match &node.value {
            Some(value) => Some((own_class, Some(Cow::Borrowed(value.as_slice())))),
            None if node.children.is_empty() => Some((own_class, None)),
            None => None,
        };
        let mask = children
            .iter()
            .fold(own.as_ref().map_or(0, |(class, _)| class.bit()), |m, c| {
                m | c.mask
            });
        Ok(Classed {
            segment: &node.segment,
            push: node.push,
            own,
            mask,
            children,
        })
    }

    /// `node`'s children, classified; without the stamps the form leaves
    /// out (and, at the top, the skipped index keys) when `filter`.
    fn children<'t>(
        &self,
        node: &'t OwnedNode,
        depth: usize,
        (cands, best): (&[&'static Rule], Option<&'static Rule>),
        at: At,
        top: bool,
        filter: bool,
    ) -> Result<Vec<Classed<'t>>, TooDeep> {
        let mut out = Vec::with_capacity(node.children.len());
        for child in &node.children {
            if filter {
                let stamp = is_stamp(&child.segment) && (top || self.form == Form::General);
                if stamp || (top && self.skip.contains(&child.segment.as_slice())) {
                    continue;
                }
            }
            let (deeper, rule) = step(cands, depth, &child.segment, best);
            let child_at = self.below(at, depth, &child.segment);
            out.push(self.node(child, depth + 1, (&deeper, rule), child_at, false)?);
        }
        Ok(out)
    }

    /// Where the child `segment` of a node whose path has `depth` segments
    /// lies, the node being at `at`. The Secret paths in a range all share
    /// the node's path below the member, so they are in order of their
    /// next segment.
    fn below(&self, at: At, depth: usize, segment: &[u8]) -> At {
        let At::Above(lo, hi) = at else {
            return at;
        };
        let i = depth - self.base;
        let range = &self.secrets[lo..hi];
        let start = lo + range.partition_point(|(p, _)| p[i].as_slice() < segment);
        let end = lo + range.partition_point(|(p, _)| p[i].as_slice() <= segment);
        if start == end {
            return At::Rules;
        }
        let here = &self.secrets[start..end];
        if here.iter().any(|(p, _)| p.len() == i + 1) {
            let credential = here.iter().all(|(_, credential)| *credential);
            At::Fixed(if credential {
                Class::Credential
            } else {
                Class::Unidentified
            })
        } else {
            At::Above(start, end)
        }
    }

    /// Classify the node at `path`, the top of a part of an object.
    fn part<'t>(
        &self,
        path: &[Vec<u8>],
        node: &'t OwnedNode,
        at: At,
    ) -> Result<Classed<'t>, TooDeep> {
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let (cands, best) = candidates(&segments);
        self.node(node, path.len(), (&cands, best), at, true)
    }
}

/// An object, classified; `None` when it is deeper than a file can be.
pub(crate) fn classify<'t>(object: &Object<'t>) -> Option<Classified<'t>> {
    let form = match object.kind {
        Kind::Script if !object.catalog => Form::Script,
        _ => Form::General,
    };
    let segments: Vec<&[u8]> = object.path.iter().map(Vec::as_slice).collect();
    let mut secrets: Vec<(Path, bool)> = Vec::new();
    if !object.catalog {
        if let Some((kind, depth)) = member_of(&segments) {
            if depth == segments.len() {
                secrets = secrets::of(kind, object.node)
                    .into_iter()
                    .map(|s| (s.path, s.credential))
                    .collect();
                secrets.sort();
            }
        }
    }
    let skip = if object.catalog {
        entry_keys(Catalog::of(object.kind))
    } else {
        Vec::new()
    };
    let walk = Walk {
        form,
        secrets: &secrets,
        base: segments.len(),
        skip: &skip,
    };
    let at = if secrets.iter().any(|(p, _)| p.is_empty()) {
        At::Fixed(Class::Unidentified)
    } else if secrets.is_empty() {
        At::Rules
    } else {
        At::Above(0, secrets.len())
    };
    let top = walk.part(&object.path, object.node, at).ok()?;
    let entry_walk = Walk {
        form,
        secrets: &[],
        base: 0,
        skip: &[],
    };
    let mut entries = Vec::with_capacity(object.entries.len());
    for (path, node) in &object.entries {
        let key = path.len().checked_sub(2).and_then(|i| path.get(i))?;
        let key = entry_key(object.kind, key)?;
        entries.push((key, entry_walk.part(path, node, At::Rules).ok()?));
    }
    Some(Classified { form, top, entries })
}

/// The index key `key` of the catalog of `kind`, as the rule table holds
/// it.
fn entry_key(kind: Kind, key: &[u8]) -> Option<&'static [u8]> {
    entry_keys(Catalog::of(kind))
        .into_iter()
        .find(|k| *k == key)
}

fn framed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// The canonical form of `node` restricted to content of `class`.
fn write(node: &Classed<'_>, class: Class, form: Form, out: &mut Vec<u8>) {
    framed(out, node.segment);
    if form == Form::Script {
        out.push(u8::from(node.push));
    }
    match &node.own {
        Some((own, Some(value))) if *own == class => {
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
    /// Whether the object holds content of `class`.
    pub fn holds(&self, class: Class) -> bool {
        self.top.holds(class) || self.entries.iter().any(|(_, e)| e.holds(class))
    }

    /// The projection of `class` (module doc): the bytes hashed.
    pub fn projection(&self, class: Class) -> Vec<u8> {
        let mut out = vec![0xff; 8];
        out.extend_from_slice(&DESIGN_REVISION_VERSION.to_be_bytes());
        out.push(class as u8);
        write(&self.top, class, self.form, &mut out);
        let entries: Vec<&(&[u8], Classed<'_>)> = self
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

    /// The design revision: `None` when the object holds unidentified
    /// content.
    pub fn revision(&self) -> Option<DesignRevision> {
        (!self.holds(Class::Unidentified))
            .then(|| DesignRevision(om_crypto::sha256(&self.projection(Class::Design))))
    }
}

#[cfg(test)]
mod tests;
