//! Reading one version of a tree: lookups, cursors over key ranges, the
//! children of a node, walks of a subtree and "did anything under this path
//! change since entry N".

use std::cmp::Ordering;
use std::ops::Bound;
use std::sync::Arc;

use super::key::{self, Key};
use super::{fault, Direction, Error, Node, Root};
use crate::page::{
    Fault, Hash, ItemView, Page, PageKind, PageNo, PageStore, Refs, OVERFLOW_DATA, SPILL_INLINE,
};

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// Reads pages for the tree. Every page is checked against what its parent
/// says it is before any of it is used: its hash (by the store, through
/// [`Page::verify`]), its kind and level, and the entry that wrote it.
pub(crate) struct Pages<'s, S: ?Sized> {
    pub(crate) store: &'s S,
}

fn malformed(page: &Page, why: &'static str) -> Error {
    fault(page.number(), page.hash(), Fault::Malformed(why))
}

/// A key or separator as its page holds it: the bytes the page holds, its
/// whole length, and the overflow pages that hold the rest of its stream.
#[derive(Clone, Copy)]
struct Stored<'p> {
    part: &'p [u8],
    len: usize,
    refs: Refs<'p>,
    /// The bytes of the stream (key and value, or separator) in the
    /// overflow pages.
    rest: usize,
    page: &'p Page,
    /// What the page is when the stored bytes break the format.
    broken: &'static str,
}

/// A key found in a tree: its bytes, or the item that holds it, read only
/// as far as a comparison needs.
#[derive(Clone)]
pub(crate) enum KeyAt {
    Bytes(Vec<u8>),
    Item(Arc<Page>, usize),
}

/// Compares the next `chunk` of a stored key, from byte `pos` of `len`, with
/// `key`. `None` while they are equal as far as both go.
fn cmp_chunk(chunk: &[u8], key: &[u8], pos: &mut usize, len: usize) -> Option<Ordering> {
    let m = chunk.len().min(key.len().saturating_sub(*pos));
    match chunk[..m].cmp(&key[*pos..*pos + m]) {
        Ordering::Equal => {}
        order => return Some(order),
    }
    if m < chunk.len() {
        // `key` ended inside the stored key, which is longer.
        return Some(Ordering::Greater);
    }
    *pos += chunk.len();
    if *pos >= len {
        return Some(len.cmp(&key.len()));
    }
    // `key` ended where the bytes so far end, and the stored key goes on.
    (*pos >= key.len()).then_some(Ordering::Greater)
}

/// The segment ends of a stored key or separator, counted as its bytes are
/// walked across pages: at most [`super::MAX_DEPTH`], with no byte after the
/// last of them, and escapes well formed (format section 2.1).
#[derive(Default)]
struct Depth {
    ends: usize,
    after_zero: bool,
}

impl Depth {
    /// Takes the next bytes; false when they break the bound.
    fn feed(&mut self, bytes: &[u8]) -> bool {
        for &b in bytes {
            if self.after_zero {
                self.after_zero = false;
                match b {
                    0x01 => self.ends += 1,
                    0xff => {}
                    _ => return false,
                }
            } else if self.ends == super::MAX_DEPTH {
                return false;
            } else if b == 0 {
                self.after_zero = true;
            }
        }
        true
    }
}

impl<'s, S: PageStore + ?Sized> Pages<'s, S> {
    fn read(&self, number: PageNo, hash: Hash) -> Result<Arc<Page>> {
        let page = self
            .store
            .read(number, &hash)
            .map_err(|f| fault(number, hash, f))?;
        if page.number() != number || page.hash() != hash {
            return Err(fault(
                number,
                hash,
                Fault::Malformed("the store returned another page"),
            ));
        }
        Ok(page)
    }

    /// The root page of a version.
    pub(crate) fn root(&self, root: Root) -> Result<Arc<Page>> {
        let page = self.read(root.page, root.hash)?;
        if page.kind() == PageKind::Overflow {
            return Err(malformed(&page, "an overflow page as a root"));
        }
        Ok(page)
    }

    /// Child `i` of the index page `parent`.
    pub(crate) fn child(&self, parent: &Page, i: usize) -> Result<Arc<Page>> {
        let entry = parent.child(i);
        self.at(
            entry.number,
            entry.hash,
            entry.stamp,
            parent.level() - 1,
            true,
        )
    }

    /// The page a parent (or, `below_root` false, a version) names with
    /// `number`, `hash` and `stamp`, at `level`. A leaf below the root is
    /// never empty (format section 2.7).
    pub(crate) fn at(
        &self,
        number: PageNo,
        hash: Hash,
        stamp: u64,
        level: u8,
        below_root: bool,
    ) -> Result<Arc<Page>> {
        let page = self.read(number, hash)?;
        let kind = if level == 0 {
            PageKind::Leaf
        } else {
            PageKind::Index
        };
        if page.kind() != kind || page.level() != level {
            return Err(malformed(&page, "not at the level its parent puts it"));
        }
        if page.written() != stamp {
            return Err(malformed(
                &page,
                "not written by the entry its parent names",
            ));
        }
        if below_root && page.count() == 0 {
            return Err(malformed(&page, "an empty leaf below the root"));
        }
        Ok(page)
    }

    /// The node with key `key` in the subtree of `page`.
    pub(crate) fn lookup(&self, mut page: Arc<Page>, key: &[u8]) -> Result<Option<Node>> {
        while page.kind() == PageKind::Index {
            let c = self.separators_below(&page, key, true)?;
            page = self.child(&page, c)?;
        }
        let i = self.items_below(&page, key, false)?;
        if i < page.count() && self.cmp_item(&page, i, key)? == Ordering::Equal {
            return Ok(Some(self.item_node(&page, i)?));
        }
        Ok(None)
    }

    /// The first key at or above `lo` in the subtree of `page`.
    pub(crate) fn first_from(&self, page: &Arc<Page>, lo: &[u8]) -> Result<Option<KeyAt>> {
        if page.kind() == PageKind::Leaf {
            let i = self.items_below(page, lo, false)?;
            return Ok((i < page.count()).then(|| KeyAt::Item(page.clone(), i)));
        }
        for c in self.separators_below(page, lo, true)?..page.count() {
            if let Some(key) = self.first_from(&self.child(page, c)?, lo)? {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }

    /// The last key below `hi` (`None`: the last key) in the subtree of
    /// `page`.
    pub(crate) fn last_before(&self, page: &Arc<Page>, hi: Option<&[u8]>) -> Result<Option<KeyAt>> {
        if page.kind() == PageKind::Leaf {
            let n = match hi {
                Some(hi) => self.items_below(page, hi, false)?,
                None => page.count(),
            };
            return Ok(n.checked_sub(1).map(|i| KeyAt::Item(page.clone(), i)));
        }
        let last = match hi {
            Some(hi) => self.separators_below(page, hi, false)?,
            None => page.count() - 1,
        };
        for c in (0..=last).rev() {
            if let Some(key) = self.last_before(&self.child(page, c)?, hi)? {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }

    /// How a found key compares with `key`.
    pub(crate) fn cmp_at(&self, at: &KeyAt, key: &[u8]) -> Result<Ordering> {
        match at {
            KeyAt::Bytes(bytes) => Ok(bytes.as_slice().cmp(key)),
            KeyAt::Item(page, i) => self.cmp_item(page, *i, key),
        }
    }

    /// A found key, whole.
    pub(crate) fn key_at(&self, at: &KeyAt) -> Result<Vec<u8>> {
        match at {
            KeyAt::Bytes(bytes) => Ok(bytes.clone()),
            KeyAt::Item(page, i) => self.item_key(page, *i),
        }
    }

    /// Overflow page `i` of a stored stream of `rest` bytes in `refs`,
    /// checked against what its owner `page` expects.
    fn overflow(&self, refs: Refs<'_>, i: usize, rest: usize, page: &Page) -> Result<Arc<Page>> {
        let (number, hash) = refs.get(i);
        let over = self.read(number, hash)?;
        let expected = (rest - i * OVERFLOW_DATA).min(OVERFLOW_DATA);
        if over.kind() != PageKind::Overflow
            || over.data().len() != expected
            || over.written() > page.written()
        {
            return Err(malformed(
                &over,
                "an overflow page that does not fit its item",
            ));
        }
        Ok(over)
    }

    /// The first `want` bytes of the part of a spilled stream that is not in
    /// its page: `rest` bytes in the overflow pages `refs` of `page`. Reads
    /// only the pages it needs.
    fn spill(&self, refs: Refs<'_>, rest: usize, want: usize, page: &Page) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(want.min(rest));
        for i in 0..refs.len() {
            if out.len() >= want {
                break;
            }
            out.extend_from_slice(self.overflow(refs, i, rest, page)?.data());
        }
        out.truncate(want);
        Ok(out)
    }

    fn stored_item<'p>(&self, page: &'p Page, i: usize) -> Stored<'p> {
        let item = page.item(i);
        Stored {
            part: item.key_part().0,
            len: item.key_len,
            refs: item.refs,
            rest: (item.key_len + item.value_len).saturating_sub(SPILL_INLINE),
            page,
            broken: "a key that is not an encoded path",
        }
    }

    fn stored_separator<'p>(&self, page: &'p Page, i: usize) -> Result<Stored<'p>> {
        let Some(separator) = page.child(i).separator else {
            return Err(malformed(page, "a first child with a separator"));
        };
        Ok(Stored {
            part: separator.part().0,
            len: separator.len,
            refs: separator.refs,
            rest: separator.len.saturating_sub(SPILL_INLINE),
            page,
            broken: "a separator that begins no path",
        })
    }

    /// How a stored key or separator compares with `key`: the bytes its page
    /// holds first, then its overflow pages one at a time, only while it is
    /// equal to `key` as far as both go. Bytes read from overflow pages are
    /// held to the depth bound as they are read.
    fn cmp_stored(&self, s: Stored<'_>, key: &[u8]) -> Result<Ordering> {
        let mut pos = 0;
        if let Some(order) = cmp_chunk(s.part, key, &mut pos, s.len) {
            return Ok(order);
        }
        let mut depth = Depth::default();
        if !depth.feed(s.part) {
            return Err(malformed(s.page, s.broken));
        }
        for i in 0..s.refs.len() {
            let over = self.overflow(s.refs, i, s.rest, s.page)?;
            let chunk = &over.data()[..over.data().len().min(s.len - pos)];
            if !depth.feed(chunk) {
                return Err(malformed(s.page, s.broken));
            }
            if let Some(order) = cmp_chunk(chunk, key, &mut pos, s.len) {
                return Ok(order);
            }
        }
        Err(malformed(s.page, "a key longer than its overflow pages"))
    }

    /// The start of item `i`'s key through the end of the segment that
    /// starts at byte `from`, reading overflow pages only until that end;
    /// `None` if the key has no segment there.
    pub(crate) fn key_through_segment(
        &self,
        page: &Page,
        i: usize,
        from: usize,
    ) -> Result<Option<Vec<u8>>> {
        let s = self.stored_item(page, i);
        let mut bytes = s.part.to_vec();
        let mut next = 0;
        loop {
            if let Some(end) = key::segment_end(&bytes, from) {
                bytes.truncate(end);
                if !key::well_formed(&bytes, true) {
                    return Err(malformed(page, "a key that is not an encoded path"));
                }
                return Ok(Some(bytes));
            }
            if bytes.len() >= s.len || next >= s.refs.len() {
                return Ok(None);
            }
            let over = self.overflow(s.refs, next, s.rest, page)?;
            let take = over.data().len().min(s.len - bytes.len());
            bytes.extend_from_slice(&over.data()[..take]);
            next += 1;
        }
    }

    /// The whole key of item `i` of a leaf.
    pub(crate) fn item_key(&self, page: &Page, i: usize) -> Result<Vec<u8>> {
        self.key_of(page, &page.item(i))
    }

    /// The whole key of `item`, an item of the leaf `page`.
    fn key_of(&self, page: &Page, item: &ItemView<'_>) -> Result<Vec<u8>> {
        let (part, complete) = item.key_part();
        if complete {
            return Ok(part.to_vec());
        }
        let rest = item.key_len + item.value_len - SPILL_INLINE;
        let tail = self.spill(item.refs, rest, item.key_len - SPILL_INLINE, page)?;
        let mut key = part.to_vec();
        key.extend_from_slice(&tail);
        if !key::well_formed(&key, true) {
            return Err(malformed(page, "a key that is not an encoded path"));
        }
        Ok(key)
    }

    /// The node item `i` of a leaf holds.
    pub(crate) fn item_node(&self, page: &Page, i: usize) -> Result<Node> {
        self.node_of(page, &page.item(i))
    }

    /// The node `item`, an item of the leaf `page`, holds.
    fn node_of(&self, page: &Page, item: &ItemView<'_>) -> Result<Node> {
        let value = match item.value_inline() {
            Some(value) => value.to_vec(),
            None => {
                let rest = item.key_len + item.value_len - SPILL_INLINE;
                let tail = self.spill(item.refs, rest, rest, page)?;
                let mut stream = Vec::with_capacity(item.inline.len() + tail.len());
                stream.extend_from_slice(item.inline);
                stream.extend_from_slice(&tail);
                stream.split_off(item.key_len)
            }
        };
        Ok(Node::from_kind(item.kind, value))
    }

    /// The whole separator of child `i` (at least 1) of an index page.
    pub(crate) fn separator(&self, page: &Page, i: usize) -> Result<Vec<u8>> {
        let s = self.stored_separator(page, i)?;
        let mut out = s.part.to_vec();
        if out.len() < s.len {
            out.extend(self.spill(s.refs, s.rest, s.rest, page)?);
            if !key::separator_ok(&out) {
                return Err(malformed(page, "a separator that begins no path"));
            }
        }
        Ok(out)
    }

    /// How item `i`'s key compares with `key`.
    pub(crate) fn cmp_item(&self, page: &Page, i: usize, key: &[u8]) -> Result<Ordering> {
        self.cmp_stored(self.stored_item(page, i), key)
    }

    /// How item `i`'s key compares with `key` where the page's own bytes
    /// decide it, as [`Pages::cmp_item`] decides it before it reads an
    /// overflow page; `None` when that needs the key's overflow pages.
    /// Reads nothing, so fails nothing.
    pub(crate) fn cmp_known(&self, page: &Page, i: usize, key: &[u8]) -> Option<Ordering> {
        let s = self.stored_item(page, i);
        cmp_chunk(s.part, key, &mut 0, s.len)
    }

    /// How the separator of child `i` compares with `key`.
    pub(crate) fn cmp_separator(&self, page: &Page, i: usize, key: &[u8]) -> Result<Ordering> {
        self.cmp_stored(self.stored_separator(page, i)?, key)
    }

    /// How many items of a leaf have a key below `key` (or equal to it).
    pub(crate) fn items_below(&self, page: &Page, key: &[u8], or_equal: bool) -> Result<usize> {
        partition(page.count(), |i| {
            let order = self.cmp_item(page, i, key)?;
            Ok(order == Ordering::Less || (or_equal && order == Ordering::Equal))
        })
    }

    /// How many separators of an index page are below `key` (or equal to
    /// it). With `or_equal`, that is the child whose range holds `key`.
    pub(crate) fn separators_below(
        &self,
        page: &Page,
        key: &[u8],
        or_equal: bool,
    ) -> Result<usize> {
        partition(page.count() - 1, |t| {
            let order = self.cmp_separator(page, t + 1, key)?;
            Ok(order == Ordering::Less || (or_equal && order == Ordering::Equal))
        })
    }
}

/// The first index in `0..n` for which `below` is false; `below` is true for
/// a prefix of the range and false after it.
pub(crate) fn partition(n: usize, mut below: impl FnMut(usize) -> Result<bool>) -> Result<usize> {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if below(mid)? {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Ok(lo)
}

/// A node and its key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub key: Key,
    pub node: Node,
}

/// One version of a tree, for reading. Nothing it returns changes later.
pub struct Tree<'s, S: ?Sized> {
    pub(crate) pages: Pages<'s, S>,
    root: Root,
    top: Arc<Page>,
}

impl<'s, S: PageStore + ?Sized> Tree<'s, S> {
    /// Opens the version `root` names, reading and checking its root page.
    pub fn open(store: &'s S, root: Root) -> Result<Tree<'s, S>> {
        let pages = Pages { store };
        let top = pages.root(root)?;
        Ok(Tree { pages, root, top })
    }

    pub fn root(&self) -> Root {
        self.root
    }

    /// The root hash: it covers the whole tree, its content and its shape,
    /// but no page numbers.
    pub fn hash(&self) -> Hash {
        self.root.hash
    }

    /// The index of the last entry that changed the tree.
    pub fn last_entry(&self) -> u64 {
        self.top.written()
    }

    /// The number of levels of pages, leaves included.
    pub fn height(&self) -> usize {
        usize::from(self.top.level()) + 1
    }

    /// The node at `path`. The empty path is the root, always a list.
    pub fn get<P: AsRef<[u8]>>(&self, path: &[P]) -> Result<Option<Node>> {
        if path.is_empty() {
            return Ok(Some(Node::List));
        }
        self.get_key(&key::encode(path))
    }

    /// The node with the encoded key `key`.
    pub fn get_key(&self, key: &[u8]) -> Result<Option<Node>> {
        self.pages.lookup(self.top.clone(), key)
    }

    /// The nodes whose keys are within `lo` and `hi`, in key order or
    /// against it.
    pub fn range(
        &self,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
        direction: Direction,
    ) -> Cursor<'_, 's, S> {
        Cursor {
            tree: self,
            lo: lo.map(<[u8]>::to_vec),
            hi: hi.map(<[u8]>::to_vec),
            direction,
            stack: Vec::new(),
            edge: Edge::First,
            state: State::Start,
        }
    }

    /// The subtree of `path` in depth-first order: the node itself, then
    /// everything under it. For the empty path, every node.
    pub fn walk<P: AsRef<[u8]>>(&self, path: &[P]) -> Cursor<'_, 's, S> {
        let start = key::encode(path);
        let end = key::subtree_end(&start);
        let lo = if path.is_empty() {
            Bound::Unbounded
        } else {
            Bound::Included(start)
        };
        Cursor {
            tree: self,
            lo,
            hi: end.map_or(Bound::Unbounded, Bound::Excluded),
            direction: Direction::Forward,
            stack: Vec::new(),
            edge: Edge::First,
            state: State::Start,
        }
    }

    /// The children of the node at `path`, from the child with segment
    /// `from` (or the first or last child when unbounded) in `direction`.
    /// Each child is found by a seek, so a child's own subtree is skipped
    /// without being read, and the only value read is the child's own.
    ///
    /// Backward, a child is found from the last key under it. When the
    /// child's own key is longer than the 1,024 bytes a spilled key keeps in
    /// its page, that last key's overflow pages are read as far as the
    /// child's key goes: the one case in which a listing reads a page of a
    /// key it does not return (`docs/server-storage-format.md` section
    /// 2.6).
    pub fn children<P: AsRef<[u8]>>(
        &self,
        path: &[P],
        from: Bound<&[u8]>,
        direction: Direction,
    ) -> Children<'_, 's, S> {
        let prefix = key::encode(path);
        let child_key = |segment: &[u8]| {
            let mut k = prefix.clone();
            key::push_segment(&mut k, segment);
            k
        };
        let end = key::subtree_end(&prefix);
        let bound = match (direction, from) {
            (Direction::Forward, Bound::Unbounded) if prefix.is_empty() => Bound::Unbounded,
            (Direction::Forward, Bound::Unbounded) => Bound::Excluded(prefix.clone()),
            (Direction::Forward, Bound::Included(s)) => Bound::Included(child_key(s)),
            (Direction::Forward, Bound::Excluded(s)) => {
                Bound::Included(key::subtree_end(&child_key(s)).unwrap_or_default())
            }
            (Direction::Backward, Bound::Unbounded) => {
                end.clone().map_or(Bound::Unbounded, Bound::Excluded)
            }
            (Direction::Backward, Bound::Included(s)) => {
                Bound::Excluded(key::subtree_end(&child_key(s)).unwrap_or_default())
            }
            (Direction::Backward, Bound::Excluded(s)) => Bound::Excluded(child_key(s)),
        };
        Children {
            tree: self,
            prefix,
            end,
            bound,
            direction,
            done: false,
            cursor: None,
        }
    }

    /// Whether anything at or under `path` changed after entry `entry`
    /// (`docs/server-storage-format.md` section 2.8). Answered from the
    /// stamps index pages hold for their children, reading at most two pages
    /// per level and no leaf. Never false when something changed; it can be
    /// true when only another path in the same leaf changed.
    pub fn changed_since<P: AsRef<[u8]>>(&self, path: &[P], entry: u64) -> Result<bool> {
        if self.top.written() <= entry {
            return Ok(false);
        }
        if path.is_empty() {
            return Ok(true);
        }
        let lo = key::encode(path);
        let hi = key::subtree_end(&lo).unwrap_or_default();
        self.changed_in(&self.top, &lo, &hi, None, None, entry)
    }

    /// Whether a child of `page` that overlaps `[lo, hi)` was written after
    /// `entry`, where `page` was written after `entry` and its own range is
    /// `[start, end)`, each the separator of child `.1` of page `.0`, or
    /// unbounded. Separators are compared, never read whole.
    fn changed_in<'p>(
        &self,
        page: &'p Page,
        lo: &[u8],
        hi: &[u8],
        start: Option<(&'p Page, usize)>,
        end: Option<(&'p Page, usize)>,
        entry: u64,
    ) -> Result<bool> {
        if page.kind() == PageKind::Leaf {
            return Ok(true);
        }
        let first = self.pages.separators_below(page, lo, true)?;
        let last = self.pages.separators_below(page, hi, false)?;
        for c in first..=last {
            if page.child(c).stamp <= entry {
                continue;
            }
            if (c > first && c < last) || page.level() == 1 {
                // A child between two separators inside `[lo, hi)`, or a
                // leaf at one of its ends, written after `entry`: counted
                // as a change, unread (format section 2.8).
                return Ok(true);
            }
            let child_start = if c == 0 { start } else { Some((page, c)) };
            let child_end = if c + 1 < page.count() {
                Some((page, c + 1))
            } else {
                end
            };
            let starts_inside = match child_start {
                Some((p, s)) => self.pages.cmp_separator(p, s, lo)? != Ordering::Less,
                None => false,
            };
            let inside = starts_inside
                && match child_end {
                    Some((p, s)) => self.pages.cmp_separator(p, s, hi)? != Ordering::Greater,
                    None => false,
                };
            if inside {
                return Ok(true);
            }
            let child = self.pages.child(page, c)?;
            if self.changed_in(&child, lo, hi, child_start, child_end, entry)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Start,
    /// A new starting bound, sought from where the cursor is
    /// ([`Cursor::seek`]).
    Seek,
    Running,
    Done,
}

/// Nodes of a key range, in key order or against it.
pub struct Cursor<'t, 's, S: ?Sized> {
    tree: &'t Tree<'s, S>,
    lo: Bound<Vec<u8>>,
    hi: Bound<Vec<u8>>,
    direction: Direction,
    /// The pages from the root to the current leaf, with the child (or item)
    /// each is at.
    stack: Vec<(Arc<Page>, usize)>,
    /// What the cursor knows of the current leaf's far edge ([`Edge`]).
    edge: Edge,
    state: State,
}

/// Whether every item of a cursor's leaf is within the far bound, as the
/// leaf's last item (first, going backward) is ([`Cursor::within`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    /// The cursor's first step in the leaf: its item is compared alone, so
    /// that a cursor that takes one step compares one item.
    First,
    /// Not compared yet: the leaf's second step compares it.
    Unknown,
    /// Every item of the leaf is within: none is compared alone.
    Within,
    /// Not shown within: each item is compared alone, as it was before
    /// the edge was looked at.
    Past,
}

impl<'t, 's, S: PageStore + ?Sized> Cursor<'t, 's, S> {
    /// Goes to the first item within the starting bound.
    fn position(&mut self) -> Result<bool> {
        let pages = &self.tree.pages;
        let forward = self.direction == Direction::Forward;
        let bound = if forward { &self.lo } else { &self.hi };
        let mut page = self.tree.top.clone();
        self.stack.clear();
        self.edge = Edge::First;
        while page.kind() == PageKind::Index {
            let c = match bound {
                Bound::Unbounded if forward => 0,
                Bound::Unbounded => page.count() - 1,
                Bound::Included(k) => pages.separators_below(&page, k, true)?,
                // Forward: the child that holds `k`. Backward: the last child
                // that starts below `k`.
                Bound::Excluded(k) => pages.separators_below(&page, k, forward)?,
            };
            let child = pages.child(&page, c)?;
            self.stack.push((page, c));
            page = child;
        }
        let n = match bound {
            Bound::Unbounded if forward => 0,
            Bound::Unbounded => page.count(),
            Bound::Included(k) => pages.items_below(&page, k, !forward)?,
            Bound::Excluded(k) => pages.items_below(&page, k, forward)?,
        };
        let count = page.count();
        if forward {
            self.stack.push((page, n));
            if n < count {
                return Ok(true);
            }
        } else if n > 0 {
            self.stack.push((page, n - 1));
            return Ok(true);
        } else {
            self.stack.push((page, 0));
        }
        self.next_leaf()
    }

    /// Starts the cursor again from `bound` (its low bound going forward,
    /// its high one backward), to be sought from where it is on its next
    /// step rather than from the root ([`Cursor::reposition`]).
    fn seek(&mut self, bound: Bound<Vec<u8>>) {
        match self.direction {
            Direction::Forward => self.lo = bound,
            Direction::Backward => self.hi = bound,
        }
        self.state = State::Seek;
    }

    /// Goes to the first item within the starting bound, as
    /// [`Cursor::position`] does, from the pages the cursor holds: from the
    /// root down, each held page is asked the choice `position` would make
    /// for the bound (no page is read), and the cursor descends anew only
    /// below the first whose choice is not the child it holds; when none
    /// is, its leaf is the one a descent reaches. So every choice is the
    /// one `position` makes and the item found is the same, on any tree,
    /// even one whose pages are out of order with their separators (review
    /// of #938: a leaf whose own items straddle the bound is no proof that
    /// the descent reaches it), and the pages kept are not read again.
    fn reposition(&mut self) -> Result<bool> {
        let forward = self.direction == Direction::Forward;
        let bound = if forward { &self.lo } else { &self.hi };
        if matches!(bound, Bound::Unbounded) {
            return self.position();
        }
        if self.stack.is_empty() {
            return self.position();
        }
        let pages = &self.tree.pages;
        // How many of the leaf's items lie before the bound, as `position`
        // counts them at its leaf.
        let below = |page: &Page| match bound {
            Bound::Included(k) => pages.items_below(page, k, !forward),
            Bound::Excluded(k) => pages.items_below(page, k, forward),
            Bound::Unbounded => Ok(0),
        };
        // The deepest page from the root whose choice of child is the one
        // the cursor holds.
        let (key, or_equal) = match bound {
            Bound::Included(k) => (k.as_slice(), true),
            Bound::Excluded(k) => (k.as_slice(), forward),
            Bound::Unbounded => return self.position(),
        };
        let choice = |page: &Page| pages.separators_below(page, key, or_equal);
        // Whether separator `i` is on the bound's near side, as
        // `separators_below` asks of each separator it compares.
        let near = |page: &Page, i: usize| -> Result<bool> {
            let order = pages.cmp_separator(page, i, key)?;
            Ok(order == Ordering::Less || (or_equal && order == Ordering::Equal))
        };
        let levels = self.stack.len() - 1;
        let mut depth = 0;
        let mut changed = None;
        while depth < levels {
            let (page, c) = &self.stack[depth];
            let (page, c) = (&**page, *c);
            // On a page whose separators its bytes prove in order, `near`
            // holds for a first run of them and not after it, and the
            // search's answer is that run's length: child `c` exactly when
            // separator `c` is near (or `c` is the first) and `c + 1` is not
            // (or `c` is the last). Asked only where every separator is in
            // the page, so that neither question can fail where the search
            // would not; any other page is searched.
            let keeps = page.separators_ordered()
                && page.separators_inline()
                && (c == 0 || near(page, c)?)
                && (c + 1 == page.count() || !near(page, c + 1)?);
            let want = if keeps { c } else { choice(page)? };
            if want != c {
                changed = Some(want);
                break;
            }
            depth += 1;
        }
        self.edge = Edge::First;
        let (page, n) = match changed {
            None => {
                // The same leaf as the descent from the root would reach.
                let (leaf, _) = self.stack.pop().expect("a stack with a leaf");
                let n = below(&leaf)?;
                (leaf, n)
            }
            Some(want) => {
                self.stack.truncate(depth + 1);
                let (parent, c) = self.stack.last_mut().expect("the page that chose anew");
                *c = want;
                let parent = parent.clone();
                let mut page = pages.child(&parent, want)?;
                while page.kind() == PageKind::Index {
                    let c = choice(&page)?;
                    let child = pages.child(&page, c)?;
                    self.stack.push((page, c));
                    page = child;
                }
                let n = below(&page)?;
                (page, n)
            }
        };
        let count = page.count();
        if forward {
            self.stack.push((page, n));
            if n < count {
                return Ok(true);
            }
        } else if n > 0 {
            self.stack.push((page, n - 1));
            return Ok(true);
        } else {
            self.stack.push((page, 0));
        }
        self.next_leaf()
    }

    /// Moves to the next item.
    fn advance(&mut self) -> Result<bool> {
        let forward = self.direction == Direction::Forward;
        if let Some((leaf, i)) = self.stack.last_mut() {
            if forward && *i + 1 < leaf.count() {
                *i += 1;
                return Ok(true);
            }
            if !forward && *i > 0 {
                *i -= 1;
                return Ok(true);
            }
        }
        self.next_leaf()
    }

    /// Leaves the current leaf for the next one that has items.
    fn next_leaf(&mut self) -> Result<bool> {
        let forward = self.direction == Direction::Forward;
        self.stack.pop();
        self.edge = Edge::First;
        loop {
            let Some((page, c)) = self.stack.last_mut() else {
                return Ok(false);
            };
            if forward && *c + 1 < page.count() {
                *c += 1;
            } else if !forward && *c > 0 {
                *c -= 1;
            } else {
                self.stack.pop();
                continue;
            }
            let (page, c) = (page.clone(), *c);
            let mut child = self.tree.pages.child(&page, c)?;
            loop {
                let count = child.count();
                if child.kind() == PageKind::Index {
                    let edge = if forward { 0 } else { count - 1 };
                    let next = self.tree.pages.child(&child, edge)?;
                    self.stack.push((child, edge));
                    child = next;
                } else {
                    if count > 0 {
                        let edge = if forward { 0 } else { count - 1 };
                        self.stack.push((child, edge));
                        return Ok(true);
                    }
                    break;
                }
            }
        }
    }

    /// Whether the item the cursor is at is within the far bound. Reads no
    /// more of the item than the comparison with the bound needs.
    ///
    /// On the leaf's second step, if its bytes prove its keys in order
    /// ([`Page::keys_ordered`]), its far edge (last item, first going
    /// backward) is compared with the bound as far as the page's bytes
    /// decide it, reading nothing ([`Pages::cmp_known`]). When they show
    /// the edge within, every item of the leaf is, and each one's own
    /// comparison would have been decided by the page's bytes as well: an
    /// item whose comparison needs its overflow pages has known bytes that
    /// start the bound, and the next item, past it where the page's bytes
    /// show it, then lies past the bound, and so would the edge. So no
    /// item's comparison is left out that could read a page or fail, and
    /// the walk gives what comparing each item gives, its errors included.
    /// Otherwise each item is compared alone.
    fn within(&mut self) -> Result<bool> {
        let Some((leaf, i)) = self.stack.last() else {
            return Ok(false);
        };
        let far = match self.direction {
            Direction::Forward => &self.hi,
            Direction::Backward => &self.lo,
        };
        let (Bound::Included(key) | Bound::Excluded(key)) = far else {
            return Ok(true);
        };
        let inside = |order: Ordering| match (self.direction, far) {
            (Direction::Forward, Bound::Included(_)) => order != Ordering::Greater,
            (Direction::Forward, _) => order == Ordering::Less,
            (Direction::Backward, Bound::Included(_)) => order != Ordering::Less,
            (Direction::Backward, _) => order == Ordering::Greater,
        };
        let pages = &self.tree.pages;
        let edge = match self.edge {
            Edge::First => Edge::Unknown,
            Edge::Unknown if leaf.keys_ordered() => {
                let at = match self.direction {
                    Direction::Forward => leaf.count().saturating_sub(1),
                    Direction::Backward => 0,
                };
                match pages.cmp_known(leaf, at, key) {
                    Some(order) if inside(order) => Edge::Within,
                    _ => Edge::Past,
                }
            }
            Edge::Unknown => Edge::Past,
            known => known,
        };
        let within = edge == Edge::Within || inside(pages.cmp_item(leaf, *i, key)?);
        self.edge = edge;
        Ok(within)
    }

    /// Moves to the next item within the bounds, without reading its
    /// value: whether there is one, at the cursor's place ([`Cursor::at`]).
    fn step(&mut self) -> Result<bool> {
        let moved = match self.state {
            State::Done => return Ok(false),
            State::Start => self.position(),
            State::Seek => self.reposition(),
            State::Running => self.advance(),
        };
        self.state = State::Running;
        let at = match moved {
            Ok(true) => self.within(),
            Ok(false) => Ok(false),
            Err(e) => Err(e),
        };
        if !matches!(at, Ok(true)) {
            self.state = State::Done;
        }
        at
    }

    /// The leaf the cursor is in and its place there, lent: after a step
    /// that found an item, that item's.
    fn at(&self) -> Option<(&Page, usize)> {
        self.stack.last().map(|(leaf, i)| (&**leaf, *i))
    }

    /// The leaf the cursor is in, to name in an error.
    fn leaf(&self) -> Option<(PageNo, Hash)> {
        self.stack
            .last()
            .map(|(page, _)| (page.number(), page.hash()))
    }
}

impl<'t, 's, S: PageStore + ?Sized> Cursor<'t, 's, S> {
    /// The next item, as [`Iterator::next`] gives it, but with its key lent
    /// to `f` rather than given: from its page when the page holds the key
    /// whole (a key of up to the 1,024 bytes a spilled item keeps in its
    /// page), else read whole first. For a reader that needs only part of
    /// each key, or a copy into its own buffer.
    pub fn next_lent<T>(&mut self, f: impl FnOnce(&[u8], Node) -> T) -> Option<Result<T>> {
        let item = self.step().and_then(|found| {
            let Some((leaf, i)) = self.at().filter(|_| found) else {
                return Ok(None);
            };
            let pages = &self.tree.pages;
            let item = leaf.item(i);
            // The key before the value, as [`Iterator::next`] reads them: a
            // key that breaks the format is the error, before any page of
            // the value is read.
            let whole;
            let key = match item.key_part() {
                (key, true) => key,
                (_, false) => {
                    whole = pages.key_of(leaf, &item)?;
                    whole.as_slice()
                }
            };
            let node = pages.node_of(leaf, &item)?;
            Ok(Some(f(key, node)))
        });
        if item.is_err() {
            self.state = State::Done;
        }
        item.transpose()
    }
}

impl<'t, 's, S: PageStore + ?Sized> Iterator for Cursor<'t, 's, S> {
    type Item = Result<Item>;

    fn next(&mut self) -> Option<Result<Item>> {
        let item = self.step().and_then(|found| {
            let Some((leaf, i)) = self.at().filter(|_| found) else {
                return Ok(None);
            };
            let pages = &self.tree.pages;
            let item = leaf.item(i);
            Ok(Some(Item {
                key: Key(pages.key_of(leaf, &item)?),
                node: pages.node_of(leaf, &item)?,
            }))
        });
        if item.is_err() {
            self.state = State::Done;
        }
        item.transpose()
    }
}

/// The children of one node: each child's segment and node. Each child is
/// found by a seek past the one before it, from where the cursor that found
/// that one is ([`Cursor::seek`]): within the same leaf, or from the
/// deepest page that still holds the next child, never from the root again
/// (#767). The cursor holds pages of the one version its tree is, which no
/// later write changes.
pub struct Children<'t, 's, S: ?Sized> {
    tree: &'t Tree<'s, S>,
    prefix: Vec<u8>,
    end: Option<Vec<u8>>,
    bound: Bound<Vec<u8>>,
    direction: Direction,
    done: bool,
    cursor: Option<Cursor<'t, 's, S>>,
}

impl<'t, 's, S: PageStore + ?Sized> Children<'t, 's, S> {
    fn step(&mut self) -> Result<Option<(Vec<u8>, Node)>> {
        let forward = self.direction == Direction::Forward;
        let below = Bound::Excluded(self.prefix.as_slice());
        let lower = if self.prefix.is_empty() {
            Bound::Unbounded
        } else {
            below
        };
        let upper = self
            .end
            .as_deref()
            .map_or(Bound::Unbounded, Bound::Excluded);
        let tree = self.tree;
        let cursor = match &mut self.cursor {
            Some(cursor) => {
                cursor.seek(self.bound.clone());
                cursor
            }
            None => {
                let bound = self.bound.as_ref().map(Vec::as_slice);
                self.cursor.insert(if forward {
                    tree.range(bound, upper, Direction::Forward)
                } else {
                    tree.range(lower, bound, Direction::Backward)
                })
            }
        };
        // Only keys are read on the way: the node read is the child's own.
        let found = cursor.step()?;
        let Some((leaf, i)) = cursor.at().filter(|_| found) else {
            return Ok(None);
        };
        let orphan = |cursor: &Cursor<'t, 's, S>| {
            let (page, hash) = cursor.leaf().unwrap_or_default();
            fault(page, hash, Fault::Malformed("a node without its parent"))
        };
        let pages = &self.tree.pages;
        let item = leaf.item(i);
        // The child's key: the found key through the end of its first
        // segment after the parent's. The page holds that unless the
        // child's own key is longer than the 1,024 bytes a spilled key keeps
        // in its page; then the found key's overflow pages are read as far
        // as the child's key goes, and no further.
        let Some(child_key) = pages.key_through_segment(leaf, i, self.prefix.len())? else {
            return Err(orphan(cursor));
        };
        let own = child_key.len() == item.key_len;
        let node = if own {
            pages.item_node(leaf, i)?
        } else if forward {
            // The child sorts before everything under it.
            return Err(orphan(cursor));
        } else {
            // Backward the child is found from the last key under it, and
            // its own node, before them, is sought from there.
            cursor.seek(Bound::Included(child_key.clone()));
            let found = cursor.step()?;
            match cursor.at().filter(|_| found) {
                Some((leaf, i)) if pages.cmp_item(leaf, i, &child_key)? == Ordering::Equal => {
                    pages.item_node(leaf, i)?
                }
                _ => return Err(orphan(cursor)),
            }
        };
        let child_key = child_key.as_slice();
        let segment = key::decode_last(&child_key[self.prefix.len()..]).unwrap_or_default();
        self.bound = if forward {
            Bound::Included(key::subtree_end(child_key).unwrap_or_default())
        } else {
            Bound::Excluded(child_key.to_vec())
        };
        Ok(Some((segment, node)))
    }
}

impl<'t, 's, S: PageStore + ?Sized> Iterator for Children<'t, 's, S> {
    type Item = Result<(Vec<u8>, Node)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let step = self.step().transpose();
        if !matches!(step, Some(Ok(_))) {
            self.done = true;
        }
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::{Builder, EntryPages, MemStore, Numbering, PageFault, KIND_VALUE};
    use crate::tree::{check, Edit};

    /// Applies an entry in a unit of its own, committed.
    fn apply(store: &MemStore, root: Root, entry: u64, edits: &[Edit]) -> Result<Root> {
        let mut unit = store.begin()?;
        let new = crate::tree::apply(store, &mut unit, Some(root), entry, edits)?;
        unit.commit()?;
        Ok(new)
    }

    /// A page store that counts the pages read from it.
    struct Counting<'s> {
        inner: &'s MemStore,
        reads: std::cell::Cell<usize>,
    }

    impl PageStore for Counting<'_> {
        type Entry<'a>
            = crate::page::MemEntry<'a>
        where
            Self: 'a;

        fn read(&self, number: PageNo, hash: &Hash) -> std::result::Result<Arc<Page>, Fault> {
            self.reads.set(self.reads.get() + 1);
            self.inner.read(number, hash)
        }

        fn trees(&self) -> Vec<(crate::page::TreeName, Root)> {
            self.inner.trees()
        }

        fn root(&self, name: &crate::page::TreeName) -> Option<Root> {
            self.inner.root(name)
        }

        fn begin(&self) -> std::result::Result<Self::Entry<'_>, crate::page::StoreError> {
            self.inner.begin()
        }
    }

    /// A tree whose node `t` has `count` children, each with three leaves
    /// of 40 bytes below it, over many leaf pages; its root, and the
    /// children's segments in order.
    fn wide(store: &MemStore, count: u32) -> (Root, Vec<Vec<u8>>) {
        let mut unit = store.begin().unwrap();
        let mut root = crate::tree::apply(
            store,
            &mut unit,
            None,
            1,
            &[Edit::list(vec![b"t".to_vec()])],
        )
        .unwrap();
        unit.commit().unwrap();
        let segments: Vec<Vec<u8>> = (0..count).map(|n| n.to_be_bytes().to_vec()).collect();
        for (n, batch) in segments.chunks(250).enumerate() {
            let mut edits = Vec::new();
            for segment in batch {
                edits.push(Edit::list(vec![b"t".to_vec(), segment.clone()]));
                for leaf in [1u8, 2, 3] {
                    edits.push(Edit::leaf(
                        vec![b"t".to_vec(), segment.clone(), vec![leaf]],
                        vec![leaf; 40],
                    ));
                }
            }
            root = apply(store, root, 2 + n as u64, &edits).unwrap();
        }
        (root, segments)
    }

    /// A listing of a node's children finds each from where the cursor
    /// that found the one before it is (#767), never from the root again:
    /// over 2,000 children of four nodes each, on many leaves, a listing in
    /// either direction reads no more pages than a walk of the node's
    /// subtree does (and a page per level for each child, as a seek from
    /// the root, would be far more), and gives each child once, in order.
    #[test]
    fn a_listing_reads_no_more_pages_than_a_walk() {
        let store = MemStore::new(Numbering::Sequential);
        let (root, segments) = wide(&store, 2_000);
        let counting = Counting {
            inner: &store,
            reads: std::cell::Cell::new(0),
        };
        let tree = Tree::open(&counting, root).unwrap();
        assert!(tree.height() >= 2, "{}", tree.height());
        let t = [b"t".to_vec()];
        let read = |run: &mut dyn FnMut()| {
            counting.reads.set(0);
            run();
            counting.reads.get()
        };
        let walk = read(&mut || assert_eq!(tree.walk(&t).count(), 1 + 4 * segments.len()));
        assert!(walk > 20, "{walk} pages: the tree should span many leaves");
        for direction in [Direction::Forward, Direction::Backward] {
            let mut listed = Vec::new();
            let pages = read(&mut || {
                listed = tree
                    .children(&t, Bound::Unbounded, direction)
                    .map(|child| child.unwrap().0)
                    .collect();
            });
            let mut expected = segments.clone();
            if direction == Direction::Backward {
                expected.reverse();
            }
            assert_eq!(listed, expected, "{direction:?}");
            assert!(
                pages <= walk + tree.height(),
                "{direction:?}: {pages} pages, a walk {walk}"
            );
        }
    }

    /// The place a listing keeps is in the version its tree is, whose pages
    /// no write changes: writes that split and merge the leaves under the
    /// listed node (new versions) while a listing of the old one runs leave
    /// what it gives that version's children, as a listing started afresh
    /// gives them.
    #[test]
    fn a_listing_keeps_to_its_version_while_writes_split_and_merge_its_leaves() {
        let store = MemStore::new(Numbering::Sequential);
        let (old, segments) = wide(&store, 1_000);
        let tree = Tree::open(&store, old).unwrap();
        let t = [b"t".to_vec()];
        let mut listing = tree.children(&t, Bound::Unbounded, Direction::Forward);
        let mut listed: Vec<Vec<u8>> = (&mut listing).take(300).map(|c| c.unwrap().0).collect();
        // Splits: many children inserted among those still to come; merges:
        // many of them deleted.
        let mut edits = Vec::new();
        for n in 300..700u32 {
            let between = [&n.to_be_bytes()[..], &[0x80]].concat();
            edits.push(Edit::leaf(vec![b"t".to_vec(), between], vec![7; 200]));
        }
        let mut newer = apply(&store, old, 100, &edits).unwrap();
        let deletes: Vec<Edit> = (700..1_000u32)
            .map(|n| Edit::Delete {
                path: vec![b"t".to_vec(), n.to_be_bytes().to_vec()],
            })
            .collect();
        newer = apply(&store, newer, 101, &deletes).unwrap();
        assert_ne!(newer, old);
        listed.extend(listing.map(|c| c.unwrap().0));
        assert_eq!(listed, segments);
        let fresh: Vec<Vec<u8>> = Tree::open(&store, old)
            .unwrap()
            .children(&t, Bound::Unbounded, Direction::Forward)
            .map(|c| c.unwrap().0)
            .collect();
        assert_eq!(fresh, segments);
    }

    /// Re-check of #697: an empty leaf below the root, which these rules
    /// never make, is refused by every read that reaches it. An edit that
    /// does not reach it keeps it; `check` finds it.
    #[test]
    fn an_empty_leaf_below_the_root_is_refused_when_reached() {
        let store = MemStore::new(Numbering::Sequential);
        let mut out = store.begin().unwrap();
        let mut write = |builder: Builder| {
            let number = out.allocate().unwrap();
            let page = builder.finish(number).unwrap();
            let r = (number, page.hash());
            out.write(Arc::new(page)).unwrap();
            r
        };
        let empty = write(Builder::new(PageKind::Leaf, 0, 1));
        let mut leaf = Builder::new(PageKind::Leaf, 0, 1);
        leaf.item(crate::page::KIND_LIST, 3, 0, b"b\x00\x01", &[]);
        let full = write(leaf);
        let mut index = Builder::new(PageKind::Index, 1, 1);
        index.child(empty.0, &empty.1, 1, None);
        index.child(full.0, &full.1, 1, Some((1, b"b", &[])));
        let top = write(index);
        out.commit().unwrap();
        let root = Root {
            page: top.0,
            hash: top.1,
        };
        let named = |e: Error| matches!(e, Error::Page(f) if f.page == empty.0);
        let tree = Tree::open(&store, root).unwrap();
        assert!(named(
            tree.walk::<Vec<u8>>(&[]).find_map(|i| i.err()).unwrap()
        ));
        assert_eq!(tree.get(&[b"b".to_vec()]).unwrap(), Some(Node::List));
        assert!(named(
            apply(&store, root, 2, &[Edit::list(vec![b"a".to_vec()])]).unwrap_err()
        ));
        let kept = apply(&store, root, 2, &[Edit::list(vec![b"c".to_vec()])]).unwrap();
        assert!(named(check(&store, kept).unwrap_err()));
    }

    /// Final re-check of #697: a spilled separator is held to the depth bound
    /// by a comparison that reads it, not only when it is read whole.
    #[test]
    fn a_separator_too_deep_is_found_while_compared() {
        let mut sep = vec![0x61u8; 4096];
        sep.extend(b"\x00\x01".repeat(65));
        let store = MemStore::new(Numbering::Sequential);
        let mut out = store.begin().unwrap();
        let number = out.allocate().unwrap();
        let mut over = Builder::new(PageKind::Overflow, 0, 1);
        over.data(&sep[crate::page::SPILL_INLINE..]);
        let over = over.finish(number).unwrap();
        let refs = [(number, over.hash())];
        out.write(Arc::new(over)).unwrap();
        out.commit().unwrap();
        let mut index = Builder::new(PageKind::Index, 1, 1);
        index.child(100, &Hash([1; 32]), 1, None);
        let inline = &sep[..crate::page::SPILL_INLINE];
        index.child(101, &Hash([2; 32]), 1, Some((sep.len(), inline, &refs)));
        let index = index.finish(99).unwrap();
        let pages = Pages { store: &store };
        let mut key = vec![0x61u8; 4096];
        key.extend(b"\x00\x01");
        let err = pages.cmp_separator(&index, 1, &key).unwrap_err();
        assert_eq!(
            err,
            fault(
                99,
                index.hash(),
                Fault::Malformed("a separator that begins no path")
            )
        );
        // Decided by the bytes its page holds, it is not read at all.
        assert_eq!(
            pages.cmp_separator(&index, 1, b"b").unwrap(),
            Ordering::Less
        );
    }

    /// What a walk gives: its items' keys and nodes, ending at an error.
    type Walked = Vec<Result<(Vec<u8>, Node)>>;

    /// What a walk of a one-leaf tree gives when each item is compared with
    /// the far bound before it is given, key before value, ending at the
    /// first item past the bound or the first error: the cursor's
    /// reference ([`Cursor::within`]).
    fn compared_each(
        store: &MemStore,
        root: Root,
        far: Bound<&[u8]>,
        direction: Direction,
    ) -> Walked {
        let pages = Pages { store };
        let leaf = pages.root(root).unwrap();
        let order: Vec<usize> = match direction {
            Direction::Forward => (0..leaf.count()).collect(),
            Direction::Backward => (0..leaf.count()).rev().collect(),
        };
        let mut out = Vec::new();
        for i in order {
            let inside = match far {
                Bound::Unbounded => Ok(true),
                Bound::Included(k) | Bound::Excluded(k) => {
                    pages.cmp_item(&leaf, i, k).map(|o| match (direction, far) {
                        (Direction::Forward, Bound::Included(_)) => o != Ordering::Greater,
                        (Direction::Forward, _) => o == Ordering::Less,
                        (Direction::Backward, Bound::Included(_)) => o != Ordering::Less,
                        (Direction::Backward, _) => o == Ordering::Greater,
                    })
                }
            };
            let item = inside.and_then(|inside| {
                inside
                    .then(|| Ok((pages.item_key(&leaf, i)?, pages.item_node(&leaf, i)?)))
                    .transpose()
            });
            match item {
                Ok(Some(item)) => out.push(Ok(item)),
                Ok(None) => break,
                Err(e) => {
                    out.push(Err(e));
                    break;
                }
            }
        }
        out
    }

    /// The same walk through the cursor, as items and lent.
    fn walked(
        store: &MemStore,
        root: Root,
        far: Bound<&[u8]>,
        direction: Direction,
    ) -> [Walked; 2] {
        let tree = Tree::open(store, root).unwrap();
        let (lo, hi) = match direction {
            Direction::Forward => (Bound::Unbounded, far),
            Direction::Backward => (far, Bound::Unbounded),
        };
        let items = tree
            .range(lo, hi, direction)
            .map(|item| item.map(|item| (item.key.0, item.node)))
            .collect();
        let mut cursor = tree.range(lo, hi, direction);
        let mut lent = Vec::new();
        while let Some(item) = cursor.next_lent(|key, node| (key.to_vec(), node)) {
            lent.push(item);
        }
        [items, lent]
    }

    /// The key of the one-segment path `segment`.
    fn key_of_segment(segment: &[u8]) -> Vec<u8> {
        key::encode(&[segment])
    }

    /// A segment that starts with 1,200 bytes `p`, so that its key's first
    /// 1,024 bytes, all that a spilled item keeps in its page, are the
    /// same for every such segment, then `tail`.
    fn long(tail: &[u8]) -> Vec<u8> {
        [&[b'p'; 1200][..], tail].concat()
    }

    /// Review of #769, rule 4: a leaf can hold, in an order its parse
    /// accepts, keys that its bytes cannot order (two spilled keys with the
    /// same first 1,024 bytes, or a spilled key before a longer whole key
    /// that starts with those bytes) and that are out of order. A cursor
    /// that took the leaf's far edge as a bound for every item gave items
    /// past the bound; it gives what comparing each item gives, in both
    /// directions.
    #[test]
    fn a_leaf_whose_bytes_do_not_order_it_is_compared_item_by_item() {
        let spilled = |tail: &[u8]| (KIND_VALUE, key_of_segment(&long(tail)), vec![7; 3000]);
        let whole = |tail: &[u8]| (KIND_VALUE, key_of_segment(&long(tail)), vec![7; 10]);
        let short = |s: &[u8]| (KIND_VALUE, key_of_segment(s), vec![7; 10]);
        let cases = [
            // Forward, bound between the tails: the second item is past it.
            (
                vec![short(b"a"), spilled(b"z"), whole(b"c")],
                b"m",
                Direction::Forward,
            ),
            (
                vec![short(b"a"), spilled(b"z"), spilled(b"c")],
                b"m",
                Direction::Forward,
            ),
            // Backward, the same mirrored.
            (
                vec![whole(b"m"), spilled(b"a"), short(b"q")],
                b"f",
                Direction::Backward,
            ),
            (
                vec![spilled(b"m"), spilled(b"a"), short(b"q")],
                b"f",
                Direction::Backward,
            ),
        ];
        for (n, (items, tail, direction)) in cases.into_iter().enumerate() {
            let store = MemStore::new(Numbering::Sequential);
            let root = crate::tree::built::tree(&store, &[items], &[]).root;
            assert!(!Pages { store: &store }.root(root).unwrap().keys_ordered());
            let bound = key_of_segment(&long(tail));
            for far in [Bound::Included(&bound[..]), Bound::Excluded(&bound[..])] {
                let expected = compared_each(&store, root, far, direction);
                assert_eq!(expected.len(), 1, "case {n}: one item before the bound");
                for got in walked(&store, root, far, direction) {
                    assert_eq!(got, expected, "case {n}, {far:?}");
                }
            }
        }
    }

    /// Review of #938 (#767): a tree whose pages each pass their own checks
    /// although they are out of order across pages, as `tree::built`
    /// writes them: a first leaf `a, z`, a second `n, o`, the separator `m`
    /// between them. A cursor that holds the first leaf and seeks `n`, as a
    /// listing seeks its next child, gives what a cursor from the root
    /// gives, in both directions, though the leaf's own items straddle the
    /// bound; and with the separator at `a`, a listing gives the children
    /// that seeking each from the root gives. On such a tree an item can
    /// come before the bound it was sought from and a listing need not end
    /// ([#998](https://github.com/OpenMaker-Dev/openmaker/issues/998)), so
    /// the first items are compared.
    #[test]
    fn a_seek_from_a_held_leaf_gives_what_the_root_gives_on_a_tree_out_of_order() {
        let store = MemStore::new(Numbering::Sequential);
        let item = |s: &[u8]| (KIND_VALUE, key_of_segment(s), vec![7; 10]);
        let tree_with = |separator: &[u8]| {
            let separator = key_of_segment(separator);
            let leaves = [vec![item(b"a"), item(b"z")], vec![item(b"n"), item(b"o")]];
            crate::tree::built::tree(&store, &leaves, &[separator.as_slice()]).root
        };
        let first = |cursor: &mut Cursor<'_, '_, MemStore>| -> Vec<Vec<u8>> {
            cursor
                .take(4)
                .map(|i| i.unwrap().key.into_bytes())
                .collect()
        };
        let tree = Tree::open(&store, tree_with(b"m")).unwrap();
        assert_eq!(tree.height(), 2);
        let n = key_of_segment(b"n");
        for direction in [Direction::Forward, Direction::Backward] {
            for bound in [Bound::Included(n.clone()), Bound::Excluded(n.clone())] {
                // In the first leaf: at `a` forward, at `z` backward.
                let mut held = tree.range(Bound::Unbounded, Bound::Unbounded, direction);
                let (steps, at) = match direction {
                    Direction::Forward => (1, b"a"),
                    Direction::Backward => (3, b"z"),
                };
                let last = (&mut held).take(steps).last().unwrap().unwrap();
                assert_eq!(last.key.as_bytes(), key_of_segment(at));
                held.seek(bound.clone());
                let from_held = first(&mut held);
                let start = bound.as_ref().map(Vec::as_slice);
                let (lo, hi) = match direction {
                    Direction::Forward => (start, Bound::Unbounded),
                    Direction::Backward => (Bound::Unbounded, start),
                };
                let from_root = first(&mut tree.range(lo, hi, direction));
                assert_eq!(from_held, from_root, "{direction:?} {bound:?}");
            }
        }
        // A listing: each child sought from where the one before was found,
        // against each sought from the root, as a listing did before #767.
        let tree = Tree::open(&store, tree_with(b"a")).unwrap();
        let root: [&[u8]; 0] = [];
        let listed: Vec<Vec<u8>> = tree
            .children(&root, Bound::Unbounded, Direction::Forward)
            .take(4)
            .map(|child| child.unwrap().0)
            .collect();
        let mut sought = Vec::new();
        let mut from = Bound::Unbounded;
        while sought.len() < 4 {
            let start = from.as_ref().map(Vec::as_slice);
            let Some(item) = tree
                .range(start, Bound::Unbounded, Direction::Forward)
                .next()
            else {
                break;
            };
            let key = item.unwrap().key.into_bytes();
            sought.push(key::decode_last(&key).unwrap());
            from = Bound::Included(key::subtree_end(&key).unwrap());
        }
        assert_eq!(listed, sought);
    }

    /// A segment of 4,200 bytes `p`, then `tail`: its key is longer than an
    /// item or a separator may keep whole in its page, so it spills, its
    /// first 1,024 bytes, the same for every such segment, in the page,
    /// where a page's bytes cannot order two of them.
    fn spilling(tail: u8) -> Vec<u8> {
        let mut segment = vec![b'p'; 4200];
        segment.push(tail);
        segment
    }

    /// A segment for `generated`: one draw in five long (`spilling`), else
    /// one to three of the letters `a` to `h`.
    fn drawn_segment(rng: &mut crate::sim::Rng) -> Vec<u8> {
        if rng.one_in(5) {
            return spilling(b'a' + rng.below(8) as u8);
        }
        let n = 1 + rng.below(3);
        (0..n).map(|_| b'a' + rng.below(8) as u8).collect()
    }

    /// `n` segments drawn, distinct, as a page can hold them in an order
    /// its own check accepts: the short ones sorted, then the long ones in
    /// the order drawn (their bytes in the page are the same, so the page
    /// cannot tell their order, and the tails are in any order).
    fn arranged(rng: &mut crate::sim::Rng, n: usize) -> Vec<Vec<u8>> {
        let drawn: Vec<Vec<u8>> = (0..n).map(|_| drawn_segment(rng)).collect();
        let (mut out, long): (Vec<_>, Vec<_>) = drawn.into_iter().partition(|s| s.len() < 4200);
        out.sort();
        out.dedup();
        for segment in long {
            if !out.contains(&segment) {
                out.push(segment);
            }
        }
        out
    }

    /// A tree of two or three levels for
    /// `a_cursor_that_seeks_from_what_it_holds_finds_what_one_from_the_root_finds`:
    /// every page valid alone (its keys or separators in an order its own
    /// check accepts, its counts and level right), each leaf's keys drawn
    /// without regard to the separators above it, so that most trees are
    /// out of order across their pages. Long keys and separators, among
    /// themselves in any order, leave some leaves and index pages unproven
    /// (`Page::keys_ordered`, `Page::separators_ordered`). Also whether an
    /// index page holds two long separators, so is unproven.
    fn generated(rng: &mut crate::sim::Rng) -> (crate::tree::built::Shape, bool) {
        fn leaf(rng: &mut crate::sim::Rng) -> crate::tree::built::Shape {
            let n = 1 + rng.below(4) as usize;
            let segments = arranged(rng, n);
            let items = segments
                .iter()
                .map(|s| (KIND_VALUE, key_of_segment(s), vec![7; 10]));
            crate::tree::built::Shape::Leaf(items.collect())
        }
        fn index(
            rng: &mut crate::sim::Rng,
            unproven: &mut bool,
            child: &mut dyn FnMut(&mut crate::sim::Rng, &mut bool) -> crate::tree::built::Shape,
        ) -> crate::tree::built::Shape {
            let n = 1 + rng.below(3) as usize;
            let separators = arranged(rng, n);
            *unproven |= separators.iter().filter(|s| s.len() >= 4200).count() > 1;
            let children = (0..=separators.len())
                .map(|_| child(rng, unproven))
                .collect();
            let separators = separators.iter().map(|s| key_of_segment(s)).collect();
            crate::tree::built::Shape::Index(children, separators)
        }
        let mut unproven = false;
        let shape = if rng.one_in(2) {
            index(rng, &mut unproven, &mut |rng, _| leaf(rng))
        } else {
            index(rng, &mut unproven, &mut |rng, unproven| {
                index(rng, unproven, &mut |rng, _| leaf(rng))
            })
        };
        (shape, unproven)
    }

    /// A bound for a seek in a tree whose keys a walk gives as `keys`: one
    /// of them, one just past it or just before it, or a key drawn afresh.
    fn drawn_bound(rng: &mut crate::sim::Rng, keys: &[Vec<u8>]) -> Vec<u8> {
        let mut key = match keys.len() {
            0 => return key_of_segment(&drawn_segment(rng)),
            n => keys[rng.below(n as u64) as usize].clone(),
        };
        match rng.below(4) {
            0 => key,
            1 => {
                key.push(0);
                key
            }
            2 => {
                key.pop();
                key
            }
            _ => key_of_segment(&drawn_segment(rng)),
        }
    }

    /// What a read gives first, errors included: no more than four items,
    /// since a listing on a tree out of order across its pages need not end
    /// ([#998](https://github.com/OpenMaker-Dev/openmaker/issues/998)).
    type Firsts = Vec<std::result::Result<Vec<u8>, String>>;

    fn firsts(items: &mut dyn Iterator<Item = Result<Item>>) -> Firsts {
        let key = |item: Result<Item>| {
            item.map(|i| i.key.into_bytes())
                .map_err(|e| format!("{e:?}"))
        };
        items.take(4).map(key).collect()
    }

    /// The contract of a cursor that seeks from the pages it holds
    /// (`Cursor::reposition`, review of #938): it finds what a cursor from
    /// the root finds, on any tree, well formed or not. Over 300 generated
    /// trees (`generated`): a cursor moved a random number of items in
    /// either direction seeks a random bound of either kind
    /// (`drawn_bound`), against a cursor from the root; and a listing of
    /// the root's children from a random bound in either direction, every
    /// child after the first sought from where the one before was found,
    /// against each child sought from the root, as listings did before
    /// #767.
    #[test]
    fn a_cursor_that_seeks_from_what_it_holds_finds_what_one_from_the_root_finds() {
        let mut rng = crate::sim::Rng::new(767);
        let (mut unproven, mut out_of_order, mut seeks, mut listings) = (0, 0, 0, 0);
        let root_path: [&[u8]; 0] = [];
        for t in 0..300 {
            let (shape, has_unproven) = generated(&mut rng);
            unproven += usize::from(has_unproven);
            let store = MemStore::new(Numbering::Sequential);
            let tree = Tree::open(&store, crate::tree::built::shaped(&store, &shape)).unwrap();
            let all = Bound::Unbounded;
            let keys: Vec<Vec<u8>> = tree
                .range(all, all, Direction::Forward)
                .map(|i| i.unwrap().key.into_bytes())
                .collect();
            out_of_order += usize::from(keys.windows(2).any(|w| w[0] >= w[1]));
            for _ in 0..24 {
                let direction = match rng.one_in(2) {
                    true => Direction::Forward,
                    false => Direction::Backward,
                };
                let key = drawn_bound(&mut rng, &keys);
                let bound = match rng.one_in(2) {
                    true => Bound::Included(key),
                    false => Bound::Excluded(key),
                };
                let mut held = tree.range(all, all, direction);
                for _ in 0..rng.below(keys.len() as u64 + 1) {
                    held.next();
                }
                held.seek(bound.clone());
                let start = bound.as_ref().map(Vec::as_slice);
                let (lo, hi) = match direction {
                    Direction::Forward => (start, all),
                    Direction::Backward => (all, start),
                };
                let from_root = firsts(&mut tree.range(lo, hi, direction));
                assert_eq!(
                    firsts(&mut held),
                    from_root,
                    "tree {t}: {direction:?} {bound:?}"
                );
                seeks += 1;
            }
            for _ in 0..4 {
                let direction = match rng.one_in(2) {
                    true => Direction::Forward,
                    false => Direction::Backward,
                };
                let segment = key::decode_last(&drawn_bound(&mut rng, &keys)).unwrap_or_default();
                let from = match rng.below(3) {
                    0 => Bound::Unbounded,
                    1 => Bound::Included(segment),
                    _ => Bound::Excluded(segment),
                };
                let segment = |child: Result<(Vec<u8>, Node)>| {
                    child.map(|c| c.0).map_err(|e| format!("{e:?}"))
                };
                let start = from.as_ref().map(Vec::as_slice);
                let listed: Firsts = tree
                    .children(&root_path, start, direction)
                    .take(4)
                    .map(segment)
                    .collect();
                let mut sought: Firsts = Vec::new();
                let mut next = from.clone();
                while sought.len() < 4 {
                    let start = next.as_ref().map(Vec::as_slice);
                    let Some(child) = tree.children(&root_path, start, direction).next() else {
                        break;
                    };
                    let child = segment(child);
                    let failed = child.is_err();
                    if let Ok(segment) = &child {
                        next = Bound::Excluded(segment.clone());
                    }
                    sought.push(child);
                    if failed {
                        break;
                    }
                }
                assert_eq!(
                    listed, sought,
                    "tree {t}: the listing {direction:?} from {from:?}"
                );
                listings += 1;
            }
        }
        assert!(
            unproven > 10,
            "{unproven} trees with an unproven index page"
        );
        assert!(
            out_of_order > 100,
            "{out_of_order} trees out of order across pages"
        );
        assert_eq!((seeks, listings), (300 * 24, 300 * 4));
    }

    /// Review of #769, rule 3: the far edge's comparison reads nothing, so
    /// a read that fails fails the item whose comparison makes it, once,
    /// and the walk ends with that error as when each item is compared. A
    /// cursor that read the edge's overflow page to compare it, dropped the
    /// failure and read the page again for the same item gave the item.
    #[test]
    fn a_failed_read_of_a_compared_key_ends_the_walk() {
        let store = MemStore::new(Numbering::Sequential);
        let items = vec![
            (KIND_VALUE, key_of_segment(b"a"), vec![7; 10]),
            (KIND_VALUE, key_of_segment(&long(b"c")), vec![7; 3000]),
        ];
        let built = crate::tree::built::tree(&store, &[items], &[]);
        let page = built.overflow[1][0];
        let bound = key_of_segment(&long(b"z"));
        let far = Bound::Excluded(&bound[..]);
        store.fail_reads(page, 1);
        let expected = compared_each(&store, built.root, far, Direction::Forward);
        assert!(matches!(expected[..], [Ok(_), Err(_)]), "{expected:?}");
        let tree = Tree::open(&store, built.root).unwrap();
        for lent in [false, true] {
            store.fail_reads(page, 1);
            let mut cursor = tree.range(Bound::Unbounded, far, Direction::Forward);
            let mut got = Vec::new();
            loop {
                let item = if lent {
                    cursor.next_lent(|key, node| (key.to_vec(), node))
                } else {
                    cursor
                        .next()
                        .map(|item| item.map(|item| (item.key.0, item.node)))
                };
                let Some(item) = item else { break };
                got.push(item);
            }
            assert_eq!(got, expected, "lent: {lent}");
        }
    }

    /// Review of #769, rule 5: a walk reads an item's key before its value,
    /// so a spilled key that breaks the format is the error, as
    /// [`Iterator::next`] reports it, before a page holding only the value
    /// fails.
    #[test]
    fn a_broken_key_is_the_error_before_its_value_is_read() {
        let store = MemStore::new(Numbering::Sequential);
        let mut key = vec![b'p'; 1024];
        key.extend_from_slice(b"\x00\x07\x00\x01");
        let value = vec![7; OVERFLOW_DATA];
        let built = crate::tree::built::tree(&store, &[vec![(KIND_VALUE, key, value)]], &[]);
        // The first overflow page holds the key's end and the value's start,
        // the second only the value's end.
        assert_eq!(built.overflow[0].len(), 2);
        store.fail_reads(built.overflow[0][1], u32::MAX);
        let tree = Tree::open(&store, built.root).unwrap();
        let next = tree.walk::<Vec<u8>>(&[]).next().unwrap().unwrap_err();
        assert!(
            matches!(
                next,
                Error::Page(PageFault {
                    fault: Fault::Malformed(_),
                    ..
                })
            ),
            "{next}"
        );
        let lent = tree
            .walk::<Vec<u8>>(&[])
            .next_lent(|_, _| ())
            .unwrap()
            .unwrap_err();
        assert_eq!(lent, next);
    }
}
