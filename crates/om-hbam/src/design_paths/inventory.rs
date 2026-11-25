//! The one way to design revisions and comparisons: a tree's design
//! objects after the identity checks ([`inventory`]).
//!
//! The checks, before anything is read from an object (a name decoded, a
//! piece classified or hashed): the sibling keys under the root, on the
//! way to each design catalog and anywhere in it are in order and
//! unique, as a file's are; every key under a design catalog's member list
//! has the shape its catalog's spec gives (a font's two bytes,
//! layout-model.md 8; a base directory's four, containers.md 8.1; a serial
//! key elsewhere, file-metadata.md 1, whose encoding is one per id),
//! whether it is a member object's or, with no children, its catalog's; no
//! two objects of one kind (and table) report one id; and no member is one
//! the decoders could read differently (`design::misread`: a script's step
//! list and step data, a layout's attribute records, object store, table
//! view and step data, looked up by number). A refusal names a kind, a
//! table and an id at most, the id only when the key has its specified
//! shape, never a key's bytes.

use std::collections::BTreeSet;
use std::fmt;
use std::ops::Deref;

use super::design::{self, DesignRevision};
use super::rules::{Group, Seg, CATALOGS};
use super::{objects, Kind, Object};
use crate::slot::be_uint;
use crate::tree::OwnedNode;

/// What an identity check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityProblem {
    /// Sibling keys out of order or repeated: two member lists, two
    /// members with one key.
    Unsorted,
    /// A key under a member list of another shape than its catalog's spec
    /// gives, whether the member has children or not.
    KeyShape,
    /// Another object of its kind (and table) reports its id.
    Duplicate,
    /// A member with a key the decoders could read as another (`[00 04]`
    /// as the step list `04`): where its step data is, is in doubt.
    Misread,
}

/// Why a tree's design objects are not trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentityError {
    /// The kind of the catalog the problem is in; `None` above every
    /// catalog.
    pub kind: Option<Kind>,
    /// A field's (or field catalog's) table's id.
    pub table: Option<u64>,
    /// The member's id, when its key has its catalog's shape.
    pub id: Option<u64>,
    pub problem: IdentityProblem,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let table = self
            .table
            .map_or_else(String::new, |t| format!(" of table {t}"));
        let what = match (self.kind, self.id) {
            (Some(kind), Some(id)) => format!("the {}{table} with id {id}", kind.name()),
            (Some(kind), None) => format!("a {}{table}", kind.name()),
            (None, _) => "the file's top-level keys".to_string(),
        };
        let why = match self.problem {
            IdentityProblem::Unsorted => "hold keys out of order or repeated",
            IdentityProblem::KeyShape => "has a key of another shape than its catalog's spec gives",
            IdentityProblem::Duplicate => "has an id another object of its kind has",
            IdentityProblem::Misread => {
                "holds a key the decoders could read as another, so where its step data is \
                 cannot be told"
            }
        };
        write!(f, "{what} {why}")
    }
}

impl std::error::Error for IdentityError {}

/// An object of a checked inventory: the only object a design revision is
/// given for. It reads as its [`Object`].
#[derive(Clone, Debug)]
pub struct Checked<'t> {
    object: Object<'t>,
}

impl<'t> Deref for Checked<'t> {
    type Target = Object<'t>;

    fn deref(&self) -> &Object<'t> {
        &self.object
    }
}

impl Checked<'_> {
    /// The object's design revision (`design`'s module doc): `None` when it
    /// holds step data or a piece no rule names.
    pub fn design_revision(&self) -> Option<DesignRevision> {
        self.object.design_revision()
    }

    /// The name, only where it is Design throughout (a catalog's always):
    /// what a report may show.
    pub fn design_name(&self) -> Option<String> {
        self.object.design_name()
    }
}

/// A member's id when its key has the shape its catalog's spec gives.
pub(crate) fn checked_id(kind: Kind, key: &[u8]) -> Option<u64> {
    match kind {
        Kind::BaseDirectory => (key.len() == 4).then(|| be_uint(key)).flatten(),
        Kind::Font => (key.len() == 2).then(|| be_uint(key)).flatten(),
        _ => crate::serial::decode(key),
    }
}

fn in_order(node: &OwnedNode) -> bool {
    node.children
        .windows(2)
        .all(|w| w[0].segment < w[1].segment)
}

/// Whether the sibling keys under the root, on the way to each design
/// catalog and anywhere in it are in order and unique.
fn ordered(root: &OwnedNode) -> Result<(), IdentityError> {
    let refuse = |kind, table| IdentityError {
        kind,
        table,
        id: None,
        problem: IdentityProblem::Unsorted,
    };
    if !in_order(root) {
        return Err(refuse(None, None));
    }
    for catalog in CATALOGS.iter().filter(|c| c.group == Group::Design) {
        let tables: Vec<Option<&[u8]>> = if catalog.per_table() {
            root.children
                .iter()
                .filter(|t| Seg::Table.matches(&t.segment))
                .map(|t| Some(t.segment.as_slice()))
                .collect()
        } else {
            vec![None]
        };
        for table in tables {
            let refused = refuse(catalog.kind, table.and_then(crate::serial::decode));
            let mut node = root;
            let mut reached = true;
            for segment in catalog.at(table) {
                if !in_order(node) {
                    return Err(refused);
                }
                match node.child(&segment) {
                    Some(child) => node = child,
                    None => {
                        reached = false;
                        break;
                    }
                }
            }
            if !reached {
                continue;
            }
            let mut stack = vec![node];
            while let Some(n) = stack.pop() {
                if !in_order(n) {
                    return Err(refused);
                }
                stack.extend(&n.children);
            }
        }
    }
    Ok(())
}

/// A tree's design objects, each checked (module doc), or why they are
/// not trusted. Nothing is read from any object before every check has
/// passed.
pub fn inventory(root: &OwnedNode) -> Result<Vec<Checked<'_>>, IdentityError> {
    ordered(root)?;
    let found = objects(root);
    let mut seen = BTreeSet::new();
    for object in &found {
        let kind = object.kind();
        let table = object.table().and_then(crate::serial::decode);
        let refuse = |id, problem| IdentityError {
            kind: Some(kind),
            table,
            id,
            problem,
        };
        let id = match &object.whole {
            // Every key under the member list: a member object's, and a
            // childless member's, which is part of the catalog's object and
            // whose key the catalog's projection holds.
            Some((_, catalog)) => {
                let members = catalog.child(&[0x05]).map_or(&[][..], |m| &m.children);
                if members
                    .iter()
                    .any(|m| checked_id(kind, &m.segment).is_none())
                {
                    return Err(refuse(None, IdentityProblem::KeyShape));
                }
                None
            }
            None => Some(
                checked_id(kind, object.key())
                    .ok_or_else(|| refuse(None, IdentityProblem::KeyShape))?,
            ),
        };
        if !seen.insert((kind, object.is_catalog(), table, id)) {
            return Err(refuse(id, IdentityProblem::Duplicate));
        }
        if !object.is_catalog() && design::misread(kind, object.node()) {
            return Err(refuse(id, IdentityProblem::Misread));
        }
    }
    Ok(found.into_iter().map(|object| Checked { object }).collect())
}
