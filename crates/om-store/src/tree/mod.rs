//! Copy-on-write B+trees of pages (`docs/server-storage.md` section 6.4).
//! The byte format and the shape rules are in
//! `docs/server-storage-format.md` section 2.
//!
//! A tree holds one key tree: nodes addressed by paths of byte strings, each
//! a list of children, a value, or both. Its [`Root`] names one fixed
//! version: [`apply`] builds the next version from an entry's edits in new
//! pages and leaves every page of the old one as it was. The shape of a tree,
//! and so its root hash, is a function of the edits applied and
//! [`crate::page::FORMAT_VERSION`] alone.

mod check;
pub mod key;
mod read;
mod write;

use std::fmt;

pub use crate::page::Root;
pub use check::{check, Report};
pub use key::Key;
pub use read::{Children, Cursor, Item, Tree};
pub use write::apply;

use crate::page::{Fault, Hash, PageFault, PageNo, StoreError, MAX_KEY, MAX_VALUE};

/// The deepest path a node may have, in segments. Bounds the ancestors one
/// edit can create (each is a prefix of the edit's path), and so what an
/// entry can make the store write.
pub const MAX_DEPTH: usize = 64;

/// What a node is: a list of children (possibly empty), a value (possibly
/// empty), or both. No file FileMaker wrote has a node that is both, but the
/// store keeps one (`docs/server-storage.md` section 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    List,
    Value(Vec<u8>),
    Both(Vec<u8>),
}

impl Node {
    /// The node's value, empty for a list.
    pub fn value(&self) -> &[u8] {
        match self {
            Node::List => &[],
            Node::Value(v) | Node::Both(v) => v,
        }
    }

    /// Whether the node is a list (it may have children).
    pub fn is_list(&self) -> bool {
        !matches!(self, Node::Value(_))
    }

    pub(crate) fn kind(&self) -> u8 {
        match self {
            Node::List => crate::page::KIND_LIST,
            Node::Value(_) => crate::page::KIND_VALUE,
            Node::Both(_) => crate::page::KIND_BOTH,
        }
    }

    pub(crate) fn from_kind(kind: u8, value: Vec<u8>) -> Node {
        match kind {
            crate::page::KIND_LIST => Node::List,
            crate::page::KIND_VALUE => Node::Value(value),
            _ => Node::Both(value),
        }
    }
}

/// One edit of an entry (`docs/server-storage.md` section 6.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// The node at `path` becomes `node`, with no children: whatever was
    /// there, and everything under it, is replaced. Missing ancestors are
    /// created as empty lists, and an ancestor that holds a value becomes
    /// one that holds both (an empty value goes, leaving an empty list), as
    /// `om_hbam::OwnedNode::put` does.
    Put { path: Vec<Vec<u8>>, node: Node },
    /// The node at `path` and everything under it are deleted. The empty
    /// path deletes every node; the root itself always exists.
    Delete { path: Vec<Vec<u8>> },
}

impl Edit {
    /// Put a leaf.
    pub fn leaf(path: Vec<Vec<u8>>, value: Vec<u8>) -> Edit {
        Edit::Put {
            path,
            node: Node::Value(value),
        }
    }

    /// Put an empty list.
    pub fn list(path: Vec<Vec<u8>>) -> Edit {
        Edit::Put {
            path,
            node: Node::List,
        }
    }

    /// Delete a subtree.
    pub fn delete(path: Vec<Vec<u8>>) -> Edit {
        Edit::Delete { path }
    }

    fn path(&self) -> &[Vec<u8>] {
        match self {
            Edit::Put { path, .. } | Edit::Delete { path } => path,
        }
    }
}

/// Why an edit is outside what the store holds (`docs/server-storage.md`
/// section 6.11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The encoded path is longer than [`MAX_KEY`] bytes.
    KeyTooLong(usize),
    /// The value is longer than [`MAX_VALUE`] bytes.
    ValueTooLong(usize),
    /// The path has more than [`MAX_DEPTH`] segments.
    TooDeep(usize),
    /// A put at the empty path: the root is always a list.
    RootPut,
}

/// Why a tree operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Edit number `edit` of the entry is outside the store's bounds.
    /// Nothing of the entry was applied.
    Refused { edit: usize, why: Refusal },
    /// The entry's index is not after the last entry that changed the tree.
    EntryOrder { entry: u64, last: u64 },
    /// A page cannot be used. It is named, with the hash its parent holds.
    Page(PageFault),
    /// The page store could not allocate or take a page.
    Store(StoreError),
    /// The tree would grow above [`crate::page::MAX_LEVEL`].
    TooTall,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused { edit, why } => write!(f, "edit {edit} refused: {why:?}"),
            Error::EntryOrder { entry, last } => {
                write!(
                    f,
                    "entry {entry} is not after entry {last}, the tree's last"
                )
            }
            Error::Page(fault) => fault.fmt(f),
            Error::Store(e) => e.fmt(f),
            Error::TooTall => f.write_str("the tree would be too tall"),
        }
    }
}

impl std::error::Error for Error {}

impl From<StoreError> for Error {
    fn from(e: StoreError) -> Error {
        Error::Store(e)
    }
}

pub(crate) fn fault(page: PageNo, hash: Hash, fault: Fault) -> Error {
    Error::Page(PageFault { page, hash, fault })
}

/// Checks an entry's edits against the store's bounds, as [`apply`] does
/// before it changes anything. A server checks this before it appends the
/// entry.
pub fn check_edits(edits: &[Edit]) -> Result<(), Error> {
    for (i, edit) in edits.iter().enumerate() {
        let put = match edit {
            Edit::Put { node, .. } => Some(node),
            Edit::Delete { .. } => None,
        };
        if let Some(why) = refusal(edit.path(), put) {
            return Err(Error::Refused { edit: i, why });
        }
    }
    Ok(())
}

/// Why [`check_edits`] refuses a put of `node` at `path`, if it does: the
/// same check on a put not yet made an [`Edit`].
pub fn check_put(path: &[Vec<u8>], node: &Node) -> Result<(), Refusal> {
    refusal(path, Some(node)).map_or(Ok(()), Err)
}

/// Why an edit at `path`, a put of `put` or a delete (`None`), is outside
/// the store's bounds.
fn refusal(path: &[Vec<u8>], put: Option<&Node>) -> Option<Refusal> {
    if path.len() > MAX_DEPTH {
        return Some(Refusal::TooDeep(path.len()));
    }
    let len = key::encoded_len(path);
    if len > MAX_KEY {
        return Some(Refusal::KeyTooLong(len));
    }
    let node = put?;
    if path.is_empty() {
        return Some(Refusal::RootPut);
    }
    if node.value().len() > MAX_VALUE {
        return Some(Refusal::ValueTooLong(node.value().len()));
    }
    None
}

/// Which way to iterate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}
