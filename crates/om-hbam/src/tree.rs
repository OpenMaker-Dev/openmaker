//! The hierarchical key space as a tree (file-format.md section 3).
//!
//! Built from any slot stream - the leaf chain of a file or a packed-slot
//! buffer from the wire - so the catalog and record decoders share one input.
//!
//! A tree takes far more memory than the slots it is read from: a node and
//! an allocation for a key of a few bytes, so a list of empty one-byte-key
//! leaves takes about 60 times its size. A reader of bytes from the network
//! uses [`read_owned`], which builds the owned tree in one pass and refuses
//! it past a memory limit, or [`SlotOrder`], which checks a slot stream as
//! reading it would without building anything.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::mem::size_of;

use crate::error::{Error, Result};
use crate::slot::{
    encode_chunk_into, minimal_code, path_string, ChunkKind, KeySegment, Slot, SlotReader, BEGIN,
    POP,
};

/// One key in the tree: its segment, an optional value, and its children in
/// key order. Values borrow the source buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node<'a> {
    pub segment: KeySegment<'a>,
    /// `Some` for a leaf slot (possibly empty) or a push slot that carries a value.
    pub value: Option<&'a [u8]>,
    /// Whether the key was pushed (opens a list, possibly empty) rather than
    /// written as a leaf. Every node with children is a push.
    pub push: bool,
    pub children: Vec<Node<'a>>,
}

impl<'a> Node<'a> {
    /// This node's segment as a numeric id.
    pub fn id(&self) -> Option<u64> {
        self.segment.as_uint()
    }

    /// The child whose segment is the number `id`.
    pub fn get(&self, id: u64) -> Option<&Node<'a>> {
        self.children.iter().find(|c| c.id() == Some(id))
    }

    /// The child with exactly these segment bytes.
    pub fn child(&self, segment: &[u8]) -> Option<&Node<'a>> {
        self.children.iter().find(|c| c.segment.0 == segment)
    }

    /// Descend through numeric segments.
    pub fn at(&self, path: &[u64]) -> Option<&Node<'a>> {
        path.iter().try_fold(self, |node, id| node.get(*id))
    }

    /// The node's value, reassembling a chunked value when needed.
    ///
    /// Values too large for one chunk, and non-ASCII text, are stored as
    /// children `0` = header and `1..=n` = data pieces (file-format.md 4.3).
    /// The header is the u32 big-endian total length, for text followed by a
    /// u32 character count and sometimes a third word (12 bytes: length,
    /// characters, 4; common in record data, file-metadata.md 6.3). Returns
    /// `None` for a node that is neither a plain value nor that pattern.
    pub fn assembled(&self) -> Option<Cow<'a, [u8]>> {
        if let Some(v) = self.value {
            return Some(Cow::Borrowed(v));
        }
        let (header, pieces) = self.children.split_first()?;
        let header_value = header.value.filter(|v| is_chunk_header(v))?;
        if header.id() != Some(0) || !header.children.is_empty() {
            return None;
        }
        join_pieces(header_value, pieces).map(Cow::Owned)
    }

    /// The value as FileMaker text: XOR 0x5A masked SCSU (file-format.md 3.5).
    pub fn text(&self) -> Option<Result<String>> {
        self.assembled()
            .map(|v| om_scsu::decode_scsu(&v).map_err(Error::from))
    }

    /// The value as a big-endian unsigned integer (1 to 8 bytes).
    pub fn uint_value(&self) -> Option<u64> {
        crate::slot::be_uint(self.value?)
    }

    /// The node's children as a packed-slot buffer relative to this node: the
    /// wire form of a downloaded subtree (network-protocol.md 5.6.6).
    ///
    /// `0x80` opens the node's list and a final `0x40` closes it; each child is
    /// a leaf chunk, or a push chunk followed by its own bracketed list
    /// (file-format.md 3.6). Chunks use the smallest code for their sizes, as
    /// FileMaker does (an empty leaf value uses value class 0). Validated
    /// byte-for-byte against 356 Download replies from FileMaker Server.
    /// Unlike the leaf chain on disk, the stream never
    /// re-pushes a path at a page boundary.
    ///
    /// Returns `None` if a key or value is too large for any chunk code.
    pub fn to_wire(&self) -> Option<Vec<u8>> {
        wire_list(self)
    }

    /// An owned copy of this node and its subtree.
    pub fn to_owned_node(&self) -> OwnedNode {
        OwnedNode {
            segment: self.segment.0.to_vec(),
            value: self.value.map(<[u8]>::to_vec),
            push: self.push,
            children: self.children.iter().map(Node::to_owned_node).collect(),
        }
    }

    /// Depth-first visit of `(path relative to this node, node)`, this node first.
    pub fn walk<'n>(&'n self, f: &mut impl FnMut(&[KeySegment<'a>], &'n Node<'a>)) {
        fn go<'n, 'a>(
            node: &'n Node<'a>,
            path: &mut Vec<KeySegment<'a>>,
            f: &mut impl FnMut(&[KeySegment<'a>], &'n Node<'a>),
        ) {
            f(path, node);
            for child in &node.children {
                path.push(child.segment);
                go(child, path, f);
                path.pop();
            }
        }
        go(self, &mut Vec::new(), f);
    }
}

/// Whether `value` is a chunked value's header: the total length alone, or
/// with the character count, or with the count and a third word.
fn is_chunk_header(value: &[u8]) -> bool {
    matches!(value.len(), 4 | 8 | 12)
}

/// A chunked value's pieces `1..=n` joined, if they are in order, hold no
/// children and add up to the total in `header` (the value's child `0`).
///
/// The total comes from the file, or from a client through a record commit
/// (op70) or an upload (op88), so nothing is reserved until the pieces
/// present are known to add up to it: a header claiming 4 GiB with no
/// pieces behind it must not reserve 4 GiB.
fn join_pieces<N: WireNode>(header: &[u8], pieces: &[N]) -> Option<Vec<u8>> {
    let total = u32::from_be_bytes(*header.first_chunk()?);
    let mut len = 0usize;
    for (n, piece) in pieces.iter().enumerate() {
        if crate::serial::decode(piece.segment()) != Some(n as u64 + 1)
            || !piece.children().is_empty()
        {
            return None;
        }
        len = len.checked_add(piece.value()?.len())?;
    }
    if len != total as usize {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    for piece in pieces {
        out.extend_from_slice(piece.value().unwrap_or_default());
    }
    Some(out)
}

/// What the wire encoder and [`join_pieces`] need from a tree node, so
/// borrowed ([`Node`]) and owned ([`OwnedNode`]) trees share one of each.
trait WireNode: Sized {
    fn segment(&self) -> &[u8];
    fn value(&self) -> Option<&[u8]>;
    fn is_push(&self) -> bool;
    fn children(&self) -> &[Self];
}

impl WireNode for Node<'_> {
    fn segment(&self) -> &[u8] {
        self.segment.0
    }
    fn value(&self) -> Option<&[u8]> {
        self.value
    }
    fn is_push(&self) -> bool {
        self.push
    }
    fn children(&self) -> &[Self] {
        &self.children
    }
}

impl WireNode for OwnedNode {
    fn segment(&self) -> &[u8] {
        &self.segment
    }
    fn value(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }
    fn is_push(&self) -> bool {
        self.push
    }
    fn children(&self) -> &[Self] {
        &self.children
    }
}

/// A node's children as one bracketed list (see [`Node::to_wire`]).
fn wire_list<N: WireNode>(node: &N) -> Option<Vec<u8>> {
    fn write<N: WireNode>(node: &N, out: &mut Vec<u8>) -> Option<()> {
        let list = node.is_push() || !node.children().is_empty();
        write_chunk(out, node.segment(), list, node.value())?;
        if list {
            write_list(node, out)?;
        }
        Some(())
    }
    fn write_list<N: WireNode>(node: &N, out: &mut Vec<u8>) -> Option<()> {
        out.push(BEGIN);
        for child in node.children() {
            write(child, out)?;
        }
        out.push(POP);
        Some(())
    }
    let mut out = Vec::new();
    write_list(node, &mut out)?;
    Some(out)
}

/// One node's own chunk in the wire form: a push chunk for a node that opens
/// a list (`list`: pushed, or holding children), with its value if it has
/// one, else a leaf chunk. Chunks use the smallest code for their sizes.
fn write_chunk(out: &mut Vec<u8>, key: &[u8], list: bool, value: Option<&[u8]>) -> Option<()> {
    if list {
        let code = minimal_code(true, key.len(), value.map(<[u8]>::len))?;
        encode_chunk_into(out, code, key, value.unwrap_or_default())
    } else {
        // An empty leaf value is written with value class 0, not as a
        // zero-length class-6 value.
        let value = value.unwrap_or_default();
        let size = (!value.is_empty()).then_some(value.len());
        let code = minimal_code(false, key.len(), size)?;
        encode_chunk_into(out, code, key, value)
    }
}

/// The wire form of a subtree ([`Node::to_wire`]) written from a depth-first
/// walk of it, one node at a time, so that the subtree need not be held as a
/// tree: the bytes `to_wire` gives for the same nodes. The subtree's own
/// node is not written, as `to_wire` writes only what is below it; its
/// children are at depth 1, theirs at 2, and so on, each after its parent
/// and in key order.
#[derive(Debug)]
pub struct WireWriter {
    out: Vec<u8>,
    /// How many lists are open: the subtree's own, and each node above the
    /// last one written that opened a list.
    open: usize,
    /// A key or value no chunk code can carry was given: no wire form.
    failed: bool,
}

impl Default for WireWriter {
    fn default() -> Self {
        WireWriter {
            out: vec![BEGIN],
            open: 1,
            failed: false,
        }
    }
}

impl WireWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Write the node at `depth` (at least 1) below the subtree's own node:
    /// its key `segment`, whether it opens a list (it was pushed or has
    /// children) and its value. A depth deeper than one below the last node
    /// that opened a list is not a walk's, and fails the whole stream.
    pub fn node(&mut self, depth: usize, segment: &[u8], list: bool, value: Option<&[u8]>) {
        if self.failed || depth == 0 || depth > self.open {
            self.failed = true;
            return;
        }
        while self.open > depth {
            self.out.push(POP);
            self.open -= 1;
        }
        if write_chunk(&mut self.out, segment, list, value).is_none() {
            self.failed = true;
            return;
        }
        if list {
            self.out.push(BEGIN);
            self.open += 1;
        }
    }

    /// The bytes written, every list closed; `None` when a key or value
    /// could not be written (as [`Node::to_wire`] gives `None`).
    pub fn finish(mut self) -> Option<Vec<u8>> {
        if self.failed {
            return None;
        }
        self.out.extend(std::iter::repeat_n(POP, self.open));
        Some(self.out)
    }
}

/// A key tree node that owns its bytes, for trees built in memory (query
/// results, records being written) rather than read from a buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnedNode {
    pub segment: Vec<u8>,
    /// `Some` for a leaf (possibly empty) or a push that carries a value.
    pub value: Option<Vec<u8>>,
    /// Whether the key opens a list (possibly empty) rather than being a leaf.
    pub push: bool,
    pub children: Vec<OwnedNode>,
}

impl OwnedNode {
    /// A leaf holding `value` (empty for a value-less key).
    pub fn leaf(segment: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            segment: segment.into(),
            value: Some(value.into()),
            push: false,
            children: Vec::new(),
        }
    }

    /// A list node holding `children`.
    pub fn list(segment: impl Into<Vec<u8>>, children: Vec<OwnedNode>) -> Self {
        Self {
            segment: segment.into(),
            value: None,
            push: true,
            children,
        }
    }

    /// The child with exactly these segment bytes. Children are kept in key
    /// order, as in a file, so this is a binary search.
    pub fn child(&self, segment: &[u8]) -> Option<&OwnedNode> {
        self.position(segment).ok().map(|i| &self.children[i])
    }

    /// The child with these segment bytes, mutably.
    pub fn child_mut(&mut self, segment: &[u8]) -> Option<&mut OwnedNode> {
        self.position(segment).ok().map(|i| &mut self.children[i])
    }

    fn position(&self, segment: &[u8]) -> std::result::Result<usize, usize> {
        self.children
            .binary_search_by(|c| c.segment.as_slice().cmp(segment))
    }

    /// The node at a path of segments below this one.
    pub fn at(&self, path: &[&[u8]]) -> Option<&OwnedNode> {
        path.iter()
            .try_fold(self, |node, segment| node.child(segment))
    }

    /// The node at a path of segments below this one, mutably.
    pub fn at_mut(&mut self, path: &[&[u8]]) -> Option<&mut OwnedNode> {
        path.iter()
            .try_fold(self, |node, segment| node.child_mut(segment))
    }

    /// The node at `path`, creating empty list nodes for the missing part.
    pub fn ensure(&mut self, path: &[&[u8]]) -> &mut OwnedNode {
        let mut node = self;
        for segment in path {
            let i = match node.position(segment) {
                Ok(i) => i,
                Err(i) => {
                    node.children
                        .insert(i, OwnedNode::list(segment.to_vec(), Vec::new()));
                    node.become_list();
                    i
                }
            };
            node = &mut node.children[i];
            // A node that gains children is a list, even if it was a leaf.
            node.become_list();
        }
        node
    }

    /// Make this node a list. A leaf's empty value goes, as a file holds a
    /// key as a value or a list, never both (hbam-writer.md 2.2), and a
    /// writer writes a list's empty value as nothing: memory keeps what
    /// the file will hold. A value that is not empty stays, for the
    /// writer to refuse (`writer::unwritable`).
    fn become_list(&mut self) {
        if !self.push && self.value.as_deref().is_some_and(<[u8]>::is_empty) {
            self.value = None;
        }
        self.push = true;
    }

    /// Put `node` at `path` (renamed to the path's last segment), replacing
    /// whatever was there and creating missing parents.
    pub fn put(&mut self, path: &[&[u8]], mut node: OwnedNode) {
        let Some((last, parents)) = path.split_last() else {
            return;
        };
        node.segment = last.to_vec();
        self.ensure(parents).set_child(node);
    }

    /// Remove and return the node at `path`, if present.
    pub fn remove(&mut self, path: &[&[u8]]) -> Option<OwnedNode> {
        let (last, parents) = path.split_last()?;
        let parent = self.at_mut(parents)?;
        let i = parent.position(last).ok()?;
        Some(parent.children.remove(i))
    }

    /// The node's value, reassembled when it is stored chunked (child `0` =
    /// u32 total length header, children `1..n` = pieces; file-format.md
    /// 4.3), as [`Node::assembled`].
    pub fn assembled(&self) -> Option<Cow<'_, [u8]>> {
        if let Some(v) = &self.value {
            return Some(Cow::Borrowed(v));
        }
        let (header, pieces) = self.children.split_first()?;
        let header_value = header.value.as_deref().filter(|v| is_chunk_header(v))?;
        if header.segment != [0] || !header.children.is_empty() {
            return None;
        }
        join_pieces(header_value, pieces).map(Cow::Owned)
    }

    /// Insert or replace the child `node` (by segment), keeping key order.
    pub fn set_child(&mut self, node: OwnedNode) {
        match self
            .children
            .binary_search_by(|c| c.segment.as_slice().cmp(&node.segment))
        {
            Ok(i) => self.children[i] = node,
            Err(i) => self.children.insert(i, node),
        }
        self.become_list();
    }

    /// The node's children in the Download wire form (see [`Node::to_wire`]).
    pub fn to_wire(&self) -> Option<Vec<u8>> {
        wire_list(self)
    }
}

/// A key tree rooted at the empty key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyTree<'a> {
    pub root: Node<'a>,
}

impl<'a> KeyTree<'a> {
    /// Build from a slot stream (e.g. [`crate::HbamFile::slots`] or a
    /// [`crate::SlotReader`] over a wire buffer).
    pub fn from_slots(slots: impl IntoIterator<Item = Result<Slot<'a>>>) -> Result<Self> {
        Self::from_slots_where(slots, |_| true)
    }

    /// Build from every slot of `readers`, in order: the tree
    /// [`Self::from_slots`] builds from their slots, without a path copied
    /// for each slot ([`SlotReader::try_for_each_slot`]), which takes most
    /// of a large catalog's build (#1108).
    pub fn from_readers(readers: impl IntoIterator<Item = SlotReader<'a>>) -> Result<Self> {
        let mut tree = KeyTree::default();
        for reader in readers {
            reader.try_for_each_slot(|path, kind, value| tree.insert(path, kind, value))?;
        }
        Ok(tree)
    }

    /// Build from the slots whose path satisfies `keep`.
    pub fn from_slots_where(
        slots: impl IntoIterator<Item = Result<Slot<'a>>>,
        mut keep: impl FnMut(&[KeySegment<'a>]) -> bool,
    ) -> Result<Self> {
        let mut tree = KeyTree::default();
        for slot in slots {
            let slot = slot?;
            if keep(&slot.path) {
                tree.insert(&slot.path, slot.kind, slot.value)?;
            }
        }
        Ok(tree)
    }

    /// Insert one slot. Slots must arrive in key order; a page boundary
    /// re-pushes the path to its first key, which lands on existing nodes.
    pub fn insert(
        &mut self,
        path: &[KeySegment<'a>],
        kind: ChunkKind,
        value: Option<&'a [u8]>,
    ) -> Result<()> {
        let out_of_order = || Error::KeyOrder {
            path: path_string(path),
        };
        let mut node = &mut self.root;
        for seg in path {
            let is_last = node.children.last().map(|c| c.segment);
            match is_last {
                Some(last) if last == *seg => {}
                Some(last) if last.0 > seg.0 => return Err(out_of_order()),
                _ => node.children.push(Node {
                    segment: *seg,
                    ..Node::default()
                }),
            }
            node = node.children.last_mut().ok_or_else(out_of_order)?;
        }
        // A key's ancestors are always pushes (they hold children).
        node.push |= kind == ChunkKind::Push;
        if let Some(v) = value {
            if node.value.is_some() {
                return Err(out_of_order());
            }
            node.value = Some(v);
        }
        Ok(())
    }

    /// The node at a numeric path.
    pub fn at(&self, path: &[u64]) -> Option<&Node<'a>> {
        self.root.at(path)
    }

    /// The node at a path of raw segment bytes (the form a wire request names).
    pub fn node(&self, path: &[&[u8]]) -> Option<&Node<'a>> {
        path.iter()
            .try_fold(&self.root, |node, segment| node.child(segment))
    }

    /// Number of nodes below the root.
    pub fn len(&self) -> usize {
        let mut n = 0;
        self.root.walk(&mut |_, _| n += 1);
        n - 1
    }

    pub fn is_empty(&self) -> bool {
        self.root.children.is_empty()
    }
}

/// What a heap allocation of `len` bytes takes: the bytes, at least 16, and
/// 16 for the allocator's own use (about what common allocators take; the
/// exact figure is the allocator's).
fn allocation(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        len.max(16) + 16
    }
}

/// What one node's own allocations take: its key, its value and the list
/// that holds its children's places.
fn own_memory(node: &OwnedNode) -> usize {
    allocation(node.segment.capacity())
        + node.value.as_ref().map_or(0, |v| allocation(v.capacity()))
        + allocation(node.children.capacity() * size_of::<OwnedNode>())
}

/// About the memory an owned tree takes: the root's place, and each node's
/// key, value and child list, the list counted with the places it holds
/// ([`read_owned`] counts the same). Walked without recursion.
pub fn memory(node: &OwnedNode) -> usize {
    let mut total = size_of::<OwnedNode>() + own_memory(node);
    let mut stack = vec![node.children.iter()];
    while let Some(children) = stack.last_mut() {
        match children.next() {
            Some(child) => {
                total += own_memory(child);
                stack.push(child.children.iter());
            }
            None => {
                stack.pop();
            }
        }
    }
    total
}

/// [`memory`], counted only as far as `limit`: `None` once the tree takes
/// more, so that measuring a large tree against a small limit stops early.
pub fn memory_within(node: &OwnedNode, limit: usize) -> Option<usize> {
    let mut total = size_of::<OwnedNode>() + own_memory(node);
    let mut stack = vec![node.children.iter()];
    while let Some(children) = stack.last_mut() {
        if total > limit {
            return None;
        }
        match children.next() {
            Some(child) => {
                total += own_memory(child);
                stack.push(child.children.iter());
            }
            None => {
                stack.pop();
            }
        }
    }
    (total <= limit).then_some(total)
}

/// Read a slot stream into an owned tree: what [`KeyTree::from_slots`] and
/// [`Node::to_owned_node`] give together, built in one pass without the
/// borrowed tree in between, and refused with [`Error::TooLarge`] once it
/// would take more than `limit` bytes of memory ([`memory`]). The check
/// follows each slot, so the tree read before a refusal passes the limit by
/// at most one slot.
pub fn read_owned<'a>(
    slots: impl IntoIterator<Item = Result<Slot<'a>>>,
    limit: usize,
) -> Result<OwnedNode> {
    let mut root = OwnedNode::default();
    let mut held = memory(&root);
    for slot in slots {
        let slot = slot?;
        held += insert_owned(&mut root, &slot.path, slot.kind, slot.value)?;
        if held > limit {
            return Err(Error::TooLarge { limit });
        }
    }
    Ok(root)
}

/// [`KeyTree::insert`] into an owned tree. Returns the memory it added.
fn insert_owned(
    root: &mut OwnedNode,
    path: &[KeySegment<'_>],
    kind: ChunkKind,
    value: Option<&[u8]>,
) -> Result<usize> {
    let out_of_order = || Error::KeyOrder {
        path: path_string(path),
    };
    let places = |node: &OwnedNode| allocation(node.children.capacity() * size_of::<OwnedNode>());
    let mut added = 0;
    let mut node = root;
    for seg in path {
        match node
            .children
            .last()
            .map(|c| c.segment.as_slice().cmp(seg.0))
        {
            Some(Ordering::Equal) => {}
            Some(Ordering::Greater) => return Err(out_of_order()),
            _ => {
                let before = places(node);
                node.children.push(OwnedNode {
                    segment: seg.0.to_vec(),
                    ..OwnedNode::default()
                });
                added += places(node) - before + allocation(seg.0.len());
            }
        }
        node = node.children.last_mut().ok_or_else(out_of_order)?;
    }
    // A key's ancestors are always pushes (they hold children).
    node.push |= kind == ChunkKind::Push;
    if let Some(v) = value {
        if node.value.is_some() {
            return Err(out_of_order());
        }
        node.value = Some(v.to_vec());
        added += allocation(v.len());
    }
    Ok(added)
}

/// The checks [`KeyTree::insert`] makes on a slot stream, without the tree:
/// keys in order at every level, and no key given two values. Slots are
/// only ever added along the tree's rightmost path, so that path is all it
/// keeps (as deep as the slots' paths, which a [`crate::SlotReader`] keeps
/// within [`crate::slot::MAX_DEPTH`]).
#[derive(Clone, Debug, Default)]
pub struct SlotOrder<'a> {
    /// The rightmost key at each level, and whether it holds a value.
    rightmost: Vec<(KeySegment<'a>, bool)>,
}

impl<'a> SlotOrder<'a> {
    /// Check one slot (a nonempty path), as inserting it into the tree of
    /// the slots before it would. Returns whether it adds a key at the top
    /// level.
    pub fn check(&mut self, path: &[KeySegment<'a>], value: Option<&[u8]>) -> Result<bool> {
        let out_of_order = || Error::KeyOrder {
            path: path_string(path),
        };
        let mut new_top = false;
        for (level, segment) in path.iter().enumerate() {
            match self
                .rightmost
                .get(level)
                .map(|(last, _)| last.0.cmp(segment.0))
            {
                Some(Ordering::Equal) => {}
                Some(Ordering::Greater) => return Err(out_of_order()),
                _ => {
                    self.rightmost.truncate(level);
                    self.rightmost.push((*segment, false));
                    new_top |= level == 0;
                }
            }
        }
        if value.is_some() {
            if let Some((_, valued)) = path
                .len()
                .checked_sub(1)
                .and_then(|last| self.rightmost.get_mut(last))
            {
                if *valued {
                    return Err(out_of_order());
                }
                *valued = true;
            }
        }
        Ok(new_top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot::SlotReader;

    /// Reading slots into an owned tree in one pass gives what the borrowed
    /// tree and `to_owned_node` give and refuses the same streams; it counts
    /// exactly what `memory` measures and stops at its limit. `SlotOrder`
    /// accepts exactly the streams the tree does.
    #[test]
    fn owned_reading_agrees_with_the_key_tree() {
        // 1000 empty leaves with two-byte keys: 3 bytes each on the wire.
        let mut tiny = vec![0x80];
        for key in 0..1000u16 {
            tiny.push(0x08);
            tiny.extend(key.to_be_bytes());
        }
        tiny.push(0x40);
        let cases: [&[&[u8]]; 6] = [
            // A nested list with values.
            &[&[
                0x80, 0x06, 0x01, 0x02, b'a', b'b', 0x20, 0x02, 0x80, 0x06, 0x03, 0x04, b'c', b'd',
                b'e', b'f', 0x40, 0x00, 0x04, 0x40,
            ]],
            // Two pages, the second re-pushing 3/16: merged.
            &[
                &[0x80, 0x20, 0x03, 0x20, 0x10, 0x01, 0x01, 0x6b],
                &[0x80, 0x20, 0x03, 0x20, 0x10, 0x01, 0x02, 0x68],
            ],
            // The same pages swapped: out of order.
            &[
                &[0x80, 0x20, 0x03, 0x20, 0x10, 0x01, 0x02, 0x68],
                &[0x80, 0x20, 0x03, 0x20, 0x10, 0x01, 0x01, 0x6b],
            ],
            // A key pushed again after its list closed, then a child below
            // the one it already holds: out of order.
            &[
                &[0x80, 0x20, 0x03, 0x80, 0x00, 0x05, 0x40],
                &[0x80, 0x20, 0x03, 0x80, 0x00, 0x04, 0x40],
            ],
            // One key given two values.
            &[&[0x80, 0x01, 0x07, 0x01, 0x01, 0x07, 0x02, 0x40]],
            &[&tiny],
        ];
        for pages in cases {
            let slots = || pages.iter().flat_map(|page| SlotReader::new(page));
            let tree = KeyTree::from_slots(slots()).map(|tree| tree.root.to_owned_node());
            // Built straight from the readers: the same tree, or the same
            // error.
            let streamed = KeyTree::from_readers(pages.iter().map(|page| SlotReader::new(page)))
                .map(|tree| tree.root.to_owned_node());
            assert_eq!(format!("{streamed:?}"), format!("{tree:?}"), "{pages:02x?}");
            let owned = read_owned(slots(), usize::MAX);
            let mut order = SlotOrder::default();
            let checked = slots().try_for_each(|slot| {
                let slot = slot?;
                order.check(&slot.path, slot.value).map(drop)
            });
            assert_eq!(checked.is_ok(), tree.is_ok(), "{pages:02x?}");
            match (tree, owned) {
                (Ok(tree), Ok(owned)) => {
                    assert_eq!(owned, tree);
                    let held = memory(&owned);
                    assert!(read_owned(slots(), held).is_ok());
                    assert!(matches!(
                        read_owned(slots(), held - 1),
                        Err(Error::TooLarge { .. })
                    ));
                }
                (Err(_), Err(owned)) => assert!(matches!(owned, Error::KeyOrder { .. })),
                (tree, owned) => panic!("{pages:02x?}: {tree:?} but {owned:?}"),
            }
        }
        // The tiny leaves take tens of times their bytes as a tree.
        let tree = read_owned(SlotReader::new(&tiny), usize::MAX).unwrap();
        assert!(memory(&tree) > 30 * tiny.len(), "{}", memory(&tree));
    }

    /// A tree built straight from readers is the one their slots build, or
    /// fails with the same error, also for a reader that starts below the
    /// root as a download's does and pops past its base: a leaf below and
    /// one beside the base, a slot after a pop past the root, a path deeper
    /// than [`crate::slot::MAX_DEPTH`], and a chunk code it cannot decode.
    #[test]
    fn readers_build_and_fail_as_their_slots_do() {
        let mut deep = vec![0x80];
        for _ in 0..=crate::slot::MAX_DEPTH {
            deep.extend([0x20, 0x01]);
        }
        // A reader's base path, its stream, and whether its slots build.
        type Case<'a> = (&'a [&'a [u8]], &'a [u8], bool);
        let base: &[&[u8]] = &[&[0x03]];
        let cases: [Case; 5] = [
            (base, &[0x80, 0x00, 0x04, 0x40, 0x00, 0x05], true),
            (base, &[0x80, 0x40, 0x40, 0x00, 0x04], false),
            (&[], &[0x80, 0x40, 0x00, 0x04], false),
            (&[], &deep, false),
            (base, &[0x80, 0x00, 0x04, 0x41], false),
        ];
        for (base, stream, builds) in cases {
            let reader =
                || SlotReader::with_path(stream, base.iter().map(|k| KeySegment(k)).collect());
            let slots = KeyTree::from_slots(reader());
            let readers = KeyTree::from_readers([reader()]);
            assert_eq!(slots.is_ok(), builds, "{stream:02x?}");
            assert_eq!(
                format!("{readers:?}"),
                format!("{slots:?}"),
                "{stream:02x?}"
            );
        }
    }

    /// A list of short leaves read within a limit of a few times its size
    /// fails with the limit error while it is read, not after its whole tree
    /// is built (#325).
    #[test]
    fn short_leaves_past_the_limit_are_refused_as_they_are_read() {
        let mut list = vec![0x80];
        for key in 0..20_000u16 {
            list.push(0x08);
            list.extend(key.to_be_bytes());
        }
        list.push(0x40);
        let limit = 4 * list.len();
        let mut read = 0;
        let slots = SlotReader::new(&list).inspect(|_| read += 1);
        let refused = read_owned(slots, limit);
        assert!(
            matches!(refused, Err(Error::TooLarge { limit: l }) if l == limit),
            "{refused:?}"
        );
        assert!(read < 20_000 / 4, "read {read} of 20000 leaves");
    }

    #[test]
    fn page_boundary_merges_and_order_is_checked() {
        // Two "pages" re-pushing 3/16 before continuing in key order.
        let page1 = [0x80, 0x20, 0x03, 0x20, 0x10, 0x01, 0x01, 0x6b];
        let page2 = [0x80, 0x20, 0x03, 0x20, 0x10, 0x01, 0x02, 0x68];
        let slots = SlotReader::new(&page1).chain(SlotReader::new(&page2));
        let tree = KeyTree::from_slots(slots).unwrap();
        let node = tree.at(&[3, 16]).unwrap();
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.get(2).unwrap().text().unwrap().unwrap(), "2");

        let back = SlotReader::new(&page2).chain(SlotReader::new(&page1));
        assert!(matches!(
            KeyTree::from_slots(back),
            Err(Error::KeyOrder { .. })
        ));
    }

    #[test]
    fn chunked_value() {
        // node 2: /0 = (len 4, chars 3), /1 = "タスク" as masked SCSU (ja sample).
        let buf = [
            0x80, 0x20, 0x02, 0x05, 0x00, 0, 0, 0, 4, 0, 0, 0, 3, 0x03, 0x01, 0x4f, 0xa5, 0xa3,
            0xb5,
        ];
        let tree = KeyTree::from_slots(SlotReader::new(&buf)).unwrap();
        assert_eq!(tree.at(&[2]).unwrap().text().unwrap().unwrap(), "タスク");
    }

    #[test]
    fn twelve_byte_chunk_headers_assemble() {
        // A custom value list's text "a¶b": header (bytes 3, characters 3,
        // 4) and one piece, masked SCSU.
        let text = om_scsu::encode_scsu("a\rb");
        let chunked = |header: Vec<u8>| {
            let node = OwnedNode::list(
                [3],
                vec![
                    OwnedNode::leaf([0], header),
                    OwnedNode::leaf([1], text.clone()),
                ],
            );
            OwnedNode::list(Vec::new(), vec![node]).to_wire().unwrap()
        };
        let twelve = chunked([3u32, 3, 4].iter().flat_map(|w| w.to_be_bytes()).collect());
        let tree = KeyTree::from_slots(SlotReader::new(&twelve)).unwrap();
        let node = tree.at(&[3]).unwrap();
        assert_eq!(node.text().unwrap().unwrap(), "a\rb");
        assert_eq!(node.to_owned_node().assembled().unwrap(), text);
        // Other header sizes are not chunked values.
        let ten = chunked(vec![0, 0, 0, 3, 0, 0, 0, 3, 0, 0]);
        let tree = KeyTree::from_slots(SlotReader::new(&ten)).unwrap();
        assert!(tree.at(&[3]).unwrap().assembled().is_none());
    }

    #[test]
    fn a_header_without_its_pieces_assembles_to_nothing() {
        // A client or file can claim a total of nearly 4 GiB with no pieces
        // behind it (#307). Reserving that total aborted the process under
        // a memory limit; the pieces must be counted before anything is
        // reserved.
        for pieces in [vec![], vec![OwnedNode::leaf([1], *b"abc")]] {
            let mut children = vec![OwnedNode::leaf([0], u32::MAX.to_be_bytes())];
            children.extend(pieces);
            let node = OwnedNode::list([3], children);
            assert!(node.assembled().is_none());
            let wire = OwnedNode::list(Vec::new(), vec![node]).to_wire().unwrap();
            let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
            assert!(tree.at(&[3]).unwrap().assembled().is_none());
        }
    }

    /// A subtree written node by node from a depth-first walk gives the
    /// bytes `to_wire` gives for it whole, for every subtree of trees read
    /// from wire bytes (nested lists, empty lists, empty and chunked
    /// values, a list of many leaves), and fails where `to_wire` does.
    #[test]
    fn a_walk_written_node_by_node_is_the_wire_form() {
        fn walk(node: &OwnedNode, depth: usize, writer: &mut WireWriter) {
            for child in &node.children {
                let list = child.push || !child.children.is_empty();
                writer.node(depth, &child.segment, list, child.value.as_deref());
                walk(child, depth + 1, writer);
            }
        }
        fn check(node: &OwnedNode) {
            let mut writer = WireWriter::new();
            walk(node, 1, &mut writer);
            assert_eq!(writer.finish(), node.to_wire(), "{node:?}");
            node.children.iter().for_each(check);
        }
        let mut many = vec![0x80];
        for key in 0..300u16 {
            many.push(0x08);
            many.extend(key.to_be_bytes());
        }
        many.push(0x40);
        let text = om_scsu::encode_scsu("a\rb");
        let chunked = OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::list(
                [3],
                vec![
                    OwnedNode::leaf(
                        [0],
                        [3u32, 3]
                            .iter()
                            .flat_map(|w| w.to_be_bytes())
                            .collect::<Vec<u8>>(),
                    ),
                    OwnedNode::leaf([1], text),
                ],
            )],
        )
        .to_wire()
        .unwrap();
        let cases: [&[u8]; 4] = [
            &[
                0x80, 0x06, 0x01, 0x02, b'a', b'b', 0x20, 0x02, 0x80, 0x06, 0x03, 0x04, b'c', b'd',
                b'e', b'f', 0x40, 0x00, 0x04, 0x40,
            ],
            &[
                0x80, 0x03, 0x03, 1, 2, 3, 4, 0x20, 0x06, 0x80, 0x40, 0x20, 0x85, 0x80, 0x01, 0x01,
                0x07, 0x40, 0x03, 0xfe, 9, 9, 9, 9, 0x40,
            ],
            &many,
            &chunked,
        ];
        for wire in cases {
            check(&read_owned(SlotReader::new(wire), usize::MAX).unwrap());
        }
        // A key no chunk code carries: no wire form either way.
        let long = OwnedNode::list(Vec::new(), vec![OwnedNode::leaf(vec![1; 300], [1])]);
        check(&long);
        assert!(long.to_wire().is_none());
        // A node deeper than one below the last list is no walk's.
        let mut writer = WireWriter::new();
        writer.node(1, &[1], false, Some(&[2]));
        writer.node(2, &[3], false, None);
        assert_eq!(writer.finish(), None);
    }

    /// `memory_within` counts what `memory` counts, and stops past its
    /// limit.
    #[test]
    fn memory_within_a_limit() {
        let mut list = vec![0x80];
        for key in 0..1000u16 {
            list.push(0x08);
            list.extend(key.to_be_bytes());
        }
        list.push(0x40);
        let tree = read_owned(SlotReader::new(&list), usize::MAX).unwrap();
        let held = memory(&tree);
        assert_eq!(memory_within(&tree, held), Some(held));
        assert_eq!(memory_within(&tree, usize::MAX), Some(held));
        assert_eq!(memory_within(&tree, held - 1), None);
        assert_eq!(memory_within(&tree, 0), None);
    }

    #[test]
    fn wire_form_round_trips() {
        // [2] subtree: leaf 3, empty push 6, push 0x85 holding leaf 1, leaf 0xfe.
        let wire = [
            0x80, 0x03, 0x03, 1, 2, 3, 4, 0x20, 0x06, 0x80, 0x40, 0x20, 0x85, 0x80, 0x01, 0x01,
            0x07, 0x40, 0x03, 0xfe, 9, 9, 9, 9, 0x40,
        ];
        let tree =
            KeyTree::from_slots(SlotReader::with_path(&wire, vec![KeySegment(&[2])])).unwrap();
        let node = tree.node(&[&[2]]).unwrap();
        assert!(node.child(&[6]).unwrap().push, "an empty list stays a push");
        assert!(!node.child(&[3]).unwrap().push);
        assert_eq!(node.to_wire().unwrap(), wire);
        assert!(tree.node(&[&[2], &[7]]).is_none());
    }
}
