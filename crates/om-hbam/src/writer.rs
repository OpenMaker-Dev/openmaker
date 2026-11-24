//! Writing a whole file from a key tree: the bulk rebuild of
//! `docs/re/hbam-writer.md` 7.2.
//!
//! The tree's chunk stream is cut into leaf pages 2, 3, ... (each re-pushing
//! the key path open at its start), the index is built bottom-up from each
//! page's first key, its top page becomes the root at page 1, and the other
//! index pages follow the leaves. Every page is stamped with a fresh SCN and
//! the superblock is marked closed cleanly. FileMaker's consistency check
//! accepts this shape, and it is the shape of FileMaker's own compacted
//! copies.
//!
//! One implementation writes every file: [`NodeWriter`] takes the tree's
//! nodes one at a time in key order and writes the pages to a target it
//! can seek in, with memory that does not grow with the file (the server
//! store's export, `docs/server-storage.md` 6.10). [`rebuild`] gives it a
//! tree held in memory and an in-memory target. What a file can hold, and
//! so what both refuse, is defined here ([`MAX_SEGMENT`], [`MAX_DEPTH`],
//! [`unwritable`]).

use std::io::Cursor;

use crate::container::PAGE_HEADER_LEN;
use crate::error::Result;
use crate::serial;
use crate::tree::OwnedNode;

mod node_writer;

pub use node_writer::NodeWriter;

/// The page size FileMaker writes.
const PAGE_SIZE: usize = 4096;
/// Usable body bytes per page.
const BODY: usize = PAGE_SIZE - PAGE_HEADER_LEN;
/// The longest value stored inline; longer ones are chunked (2.3).
const MAX_INLINE_VALUE: usize = 1000;
/// The longest key segment a file can hold (a length byte below `0xDC`,
/// 2.2). Segments are at least one byte long.
pub const MAX_SEGMENT: usize = 219;
/// The deepest key path a written file may hold (16 segments, 2.3): a leaf
/// may sit at depth 16, a list or chunked value at depth 15 at most. The
/// one definition of this limit: [`unwritable`] and `om-query`'s program
/// bounds use it too.
pub const MAX_DEPTH: usize = 16;

/// Why the writer cannot write node `n` itself at `depth` (1 for a child
/// of the root), its open key path taking `open` bytes when a page re-pushes
/// it (2.1); its children are checked on their own.
fn refusal(n: &OwnedNode, depth: usize, open: usize) -> Option<&'static str> {
    refusal_of(
        &n.segment,
        n.push || !n.children.is_empty(),
        n.value.as_deref().unwrap_or_default(),
        depth,
        open,
    )
}

/// [`refusal`] of a key with this `segment`, a list or not, holding
/// `value` (empty for none). A page holds the re-pushed path and at least
/// one item, so an item that does not fit after its open path fits on no
/// page.
fn refusal_of(
    segment: &[u8],
    list: bool,
    value: &[u8],
    depth: usize,
    open: usize,
) -> Option<&'static str> {
    if segment.is_empty() || segment.len() > MAX_SEGMENT || depth > MAX_DEPTH {
        return Some("key segment length or depth out of range");
    }
    // A push and its begin are one unit.
    let opened = open + push_len(segment);
    if list {
        if !value.is_empty() {
            return Some("a list key cannot carry a value");
        }
        if depth == MAX_DEPTH {
            return Some("a list at the deepest level");
        }
        if opened + 1 > BODY {
            return Some("a key path too long to paginate");
        }
    } else if value.len() > MAX_INLINE_VALUE {
        if depth == MAX_DEPTH {
            return Some("a chunked value at the deepest level");
        }
        let Ok(pieces) = u32::try_from(value.len().div_ceil(MAX_INLINE_VALUE)) else {
            return Some("value over 4 GiB");
        };
        if u32::try_from(value.len()).is_err() {
            return Some("value over 4 GiB");
        }
        // The longest piece key is the last one's.
        let piece = leaf_len(&serial::encode(u64::from(pieces)), MAX_INLINE_VALUE);
        if opened + 1 > BODY || opened + piece > BODY {
            return Some("a key path too long to paginate");
        }
    } else if open + leaf_len(segment, value.len()) > BODY {
        return Some("a key path too long to paginate");
    }
    None
}

/// A node [`rebuild`] cannot write: its key path and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unwritable {
    pub path: Vec<Vec<u8>>,
    pub reason: &'static str,
}

/// The nodes of `node`, placed at key path `parents` + its own segment,
/// that [`rebuild`] cannot write (the ancestors in `parents` included, as
/// lists), in key order; a refused node's children are not visited. Empty
/// when the subtree can be stored: a writer refuses exactly what this
/// reports (hbam-writer.md 2.2, 2.3, 2.1). Stops after `limit` nodes.
pub fn unwritable(parents: &[&[u8]], node: &OwnedNode, limit: usize) -> Vec<Unwritable> {
    fn visit(
        n: &OwnedNode,
        path: &mut Vec<Vec<u8>>,
        open: usize,
        limit: usize,
        out: &mut Vec<Unwritable>,
    ) {
        if out.len() >= limit {
            return;
        }
        path.push(n.segment.clone());
        if let Some(reason) = refusal(n, path.len(), open) {
            out.push(Unwritable {
                path: path.clone(),
                reason,
            });
        } else if n.push || !n.children.is_empty() {
            let open = open + push_len(&n.segment);
            for child in &n.children {
                visit(child, path, open, limit, out);
            }
        }
        path.pop();
    }
    let mut out = Vec::new();
    let mut path = Vec::new();
    let mut open = 0;
    for parent in parents {
        let list = OwnedNode {
            push: true,
            ..OwnedNode::list(parent.to_vec(), Vec::new())
        };
        path.push(parent.to_vec());
        if let Some(reason) = refusal(&list, path.len(), open) {
            out.push(Unwritable {
                path: path.clone(),
                reason,
            });
            return out;
        }
        open += push_len(parent);
    }
    visit(node, &mut path, open, limit, &mut out);
    out
}

/// Whether `node` holds a value: an inline one, or one chunked into pieces
/// (2.3, as a file stores a value over 1000 bytes). A key is either a value
/// or a list (2.2: "Can't have both data and children"), so nothing may be
/// put beneath a node that holds a value: it would become a list that
/// carries a value, which no file holds, or gain children among its pieces,
/// changing the value.
pub fn holds_value(node: &OwnedNode) -> bool {
    node.value.as_deref().is_some_and(|value| !value.is_empty())
        || (node.value.is_none() && !node.children.is_empty() && node.assembled().is_some())
}

/// The path of the first node along `path` in `root`, the node at `path`
/// included, that [`holds_value`]: a write that creates or extends lists
/// through `path` (as `OwnedNode::ensure` and `OwnedNode::put` do) would
/// change it. `None` when the path passes through lists and missing nodes
/// only.
pub fn value_on_path(root: &OwnedNode, path: &[&[u8]]) -> Option<Vec<Vec<u8>>> {
    let mut node = root;
    for (depth, segment) in path.iter().enumerate() {
        node = node.child(segment)?;
        if holds_value(node) {
            return Some(path[..=depth].iter().map(|s| s.to_vec()).collect());
        }
    }
    None
}

/// What putting `node` (named by its own segment) beneath `parents` in
/// `root` would leave that [`rebuild`] cannot write or that changes a value
/// the put does not name, checked against the tree as it is, as
/// `OwnedNode::put` puts it (missing parents become lists, existing ones
/// stay or become lists): an existing parent that [`holds_value`], then
/// what [`unwritable`] reports for the node under its parents. Empty when
/// the put is safe. The node that `node` replaces is the put's to change.
pub fn put_refusals(
    root: &OwnedNode,
    parents: &[&[u8]],
    node: &OwnedNode,
    limit: usize,
) -> Vec<Unwritable> {
    if let Some(path) = value_on_path(root, parents) {
        return vec![Unwritable {
            path,
            reason: "a value cannot become a list",
        }];
    }
    unwritable(parents, node, limit)
}

/// Every node of `root`, a file's whole key tree, that [`rebuild`] cannot
/// write ([`unwritable`]), at most `limit` of them.
pub fn unwritable_in_tree(root: &OwnedNode, limit: usize) -> Vec<Unwritable> {
    let mut out = Vec::new();
    for child in &root.children {
        out.extend(unwritable(&[], child, limit - out.len()));
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// The bytes a chunk's key takes: a length byte unless it is 1 to 3 bytes
/// long (the size classes of `minimal_code`).
fn key_len(segment: &[u8]) -> usize {
    match segment.len() {
        n @ 1..=3 => n,
        n => 1 + n,
    }
}

/// The length of the push chunk of `segment` (`node_writer::put_push`).
fn push_len(segment: &[u8]) -> usize {
    1 + key_len(segment)
}

/// The length of the leaf chunk of `segment` with a value of `value` bytes
/// (at most 65,535).
fn leaf_len(segment: &[u8], value: usize) -> usize {
    let size = match value {
        0..=2 | 4 | 6 | 8 => 0,
        3..=255 => 1,
        _ => 2,
    };
    1 + key_len(segment) + size + value
}

/// Write `root`, the whole key tree of a file, as a complete file. Page 0 is
/// `superblock` (the original file's page 0) with a new SCN and the
/// clean-close flag set.
///
/// The tree goes through [`NodeWriter`], a node at a time in the order of
/// a depth-first walk, into a buffer: a node with children is given as a
/// list, and children out of key order, or two with one segment, are
/// refused.
pub fn rebuild(root: &OwnedNode, superblock: &[u8]) -> Result<Vec<u8>> {
    fn walk<'a>(
        node: &'a OwnedNode,
        path: &mut Vec<&'a [u8]>,
        writer: &mut NodeWriter<Cursor<Vec<u8>>>,
    ) -> Result<()> {
        path.push(&node.segment);
        let list = node.push || !node.children.is_empty();
        // A refused node stops the walk here, so a path never grows past
        // one segment beyond `MAX_DEPTH`.
        writer.node(path, list, node.value.as_deref())?;
        for child in &node.children {
            walk(child, path, writer)?;
        }
        path.pop();
        Ok(())
    }
    let mut writer = NodeWriter::new(Cursor::new(Vec::new()), superblock)?;
    let mut path = Vec::new();
    for child in &root.children {
        walk(child, &mut path, &mut writer)?;
    }
    Ok(writer.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::node_writer::{put_leaf, put_push, SCN};
    use super::*;

    #[test]
    fn long_values_are_chunked_with_serial_piece_keys() {
        // 200,500 bytes: 201 pieces, so piece 128 is keyed `80 00`.
        let value: Vec<u8> = (0..200_500u32).map(|i| (i % 251) as u8).collect();
        let root = OwnedNode::list(Vec::new(), vec![OwnedNode::leaf(vec![0x05], value.clone())]);
        let superblock = {
            let mut page = vec![0u8; PAGE_SIZE];
            page[0x0f..0x14].copy_from_slice(crate::container::MAGIC);
            page[0x14..0x18].copy_from_slice(&(PAGE_SIZE as u32).to_be_bytes());
            page
        };
        let file = crate::HbamFile::from_bytes(rebuild(&root, &superblock).unwrap()).unwrap();
        let node = file.subtree(&[&[0x05]]).unwrap().unwrap();
        assert_eq!(node.children.len(), 202);
        assert_eq!(node.children[128].segment.0, [0x80, 0x00]);
        assert_eq!(node.assembled().unwrap().as_ref(), value.as_slice());
        // A stored change number with no room for the pages is refused.
        let mut full = superblock.clone();
        full[SCN..SCN + 8].copy_from_slice(&(u64::MAX - 3).to_be_bytes());
        assert!(rebuild(&root, &full).is_err());
    }

    #[test]
    fn unwritable_reports_what_rebuild_refuses() {
        // The page-room check counts chunk bytes as the encoder writes them.
        for key in 1..=MAX_SEGMENT {
            let segment = vec![7; key];
            let mut chunk = Vec::new();
            put_push(&segment, &mut chunk);
            assert_eq!(push_len(&segment), chunk.len());
            for value in (0..=20).chain([254, 255, 256, MAX_INLINE_VALUE]) {
                chunk.clear();
                put_leaf(&segment, &vec![1; value], &mut chunk);
                assert_eq!(leaf_len(&segment, value), chunk.len());
            }
        }
        let superblock = {
            let mut page = vec![0u8; PAGE_SIZE];
            page[0x0f..0x14].copy_from_slice(crate::container::MAGIC);
            page[0x14..0x18].copy_from_slice(&(PAGE_SIZE as u32).to_be_bytes());
            page
        };
        let leaf = |segment: Vec<u8>, value: usize| OwnedNode::leaf(segment, vec![3; value]);
        // A path of `depth` lists of `width`-byte segments ending in `last`.
        let deep = |depth: usize, width: usize, last: OwnedNode| {
            (1..depth).rev().fold(last, |child, d| {
                OwnedNode::list(vec![d as u8; width], vec![child])
            })
        };
        let cases = [
            (leaf(vec![1; MAX_SEGMENT], 0), true),
            (leaf(vec![1; MAX_SEGMENT + 1], 0), false),
            (leaf(Vec::new(), 1), false),
            (deep(MAX_DEPTH, 1, leaf(vec![1], 1)), true),
            (deep(MAX_DEPTH + 1, 1, leaf(vec![1], 1)), false),
            (deep(MAX_DEPTH, 1, leaf(vec![1], 1001)), false),
            (deep(MAX_DEPTH - 1, 1, leaf(vec![1], 5000)), true),
            // Fifteen 219-byte lists leave no page room for a 1000-byte
            // value under a long key, but do for a short value.
            (
                deep(MAX_DEPTH, MAX_SEGMENT, leaf(vec![1; MAX_SEGMENT], 1000)),
                false,
            ),
            (
                deep(MAX_DEPTH, MAX_SEGMENT, leaf(vec![1; MAX_SEGMENT], 10)),
                true,
            ),
            (deep(MAX_DEPTH - 1, MAX_SEGMENT, leaf(vec![1], 2000)), false),
        ];
        for (i, (node, writable)) in cases.into_iter().enumerate() {
            let root = OwnedNode::list(Vec::new(), vec![node.clone()]);
            let found = unwritable_in_tree(&root, 10);
            assert_eq!(found.is_empty(), writable, "case {i}: {found:?}");
            assert_eq!(rebuild(&root, &superblock).is_ok(), writable, "case {i}");
        }
        // Against a real tree: beneath a committed value, inline or chunked,
        // nothing may be put; beneath a list or a missing node, it may.
        let tree = OwnedNode::list(
            Vec::new(),
            vec![
                leaf(vec![1], 4),
                OwnedNode::list(
                    vec![2],
                    vec![
                        OwnedNode::leaf(vec![0], 3000u32.to_be_bytes().to_vec()),
                        leaf(vec![1], 1000),
                        leaf(vec![2], 1000),
                        leaf(vec![3], 1000),
                    ],
                ),
                OwnedNode::list(vec![3], vec![leaf(vec![1], 4)]),
            ],
        );
        for (parents, refused) in [
            (&[&[1u8][..]][..], true),
            (&[&[2u8][..]][..], true),
            (&[&[1u8][..], &[5u8][..]][..], true),
            (&[&[3u8][..]][..], false),
            (&[&[4u8][..], &[5u8][..]][..], false),
        ] {
            let found = put_refusals(&tree, parents, &leaf(vec![9], 1), 10);
            assert_eq!(!found.is_empty(), refused, "{parents:?}: {found:?}");
        }
        // A subtree under parents that are already too deep.
        let parents: Vec<&[u8]> = vec![&[1]; MAX_DEPTH];
        assert_eq!(
            unwritable(&parents, &leaf(vec![2], 1), 10)[0].path.len(),
            MAX_DEPTH
        );
    }
}
