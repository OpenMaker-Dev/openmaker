//! Reading one version of a tree: lookups, cursors over key ranges, the
//! children of a node, walks of a subtree and "did anything under this path
//! change since entry N".

use std::cmp::Ordering;
use std::ops::Bound;
use std::sync::Arc;

use super::key::{self, Key};
use super::{fault, Direction, Error, Node, Root};
use crate::page::{
    Fault, Hash, Page, PageKind, PageNo, PageStore, Refs, OVERFLOW_DATA, SPILL_INLINE,
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
        let item = page.item(i);
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
        let item = page.item(i);
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
            leaf_within: None,
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
            leaf_within: None,
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
    /// Whether every item of the current leaf is within the far bound, as
    /// its last item (first, going backward) is: `None` until compared.
    leaf_within: Option<bool>,
    state: State,
}

impl<'t, 's, S: PageStore + ?Sized> Cursor<'t, 's, S> {
    /// Goes to the first item within the starting bound.
    fn position(&mut self) -> Result<bool> {
        let pages = &self.tree.pages;
        let forward = self.direction == Direction::Forward;
        let bound = if forward { &self.lo } else { &self.hi };
        let mut page = self.tree.top.clone();
        self.stack.clear();
        self.leaf_within = None;
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
        self.leaf_within = None;
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

    /// The item the cursor is at, if it is within the far bound. Reads no
    /// more of an item than the comparison with the bound needs. A leaf's
    /// keys are in order, so when its last item (first, going backward) is
    /// within the bound, every item of it is: that item is compared once,
    /// on the leaf's first step, and each item alone only when it is not
    /// within, or the comparison failed (the failure is then the item's own,
    /// if its comparison fails too).
    fn within(&mut self) -> Result<Option<(Arc<Page>, usize)>> {
        let Some((leaf, i)) = self.stack.last() else {
            return Ok(None);
        };
        let pages = &self.tree.pages;
        let compare = |at: usize| -> Result<bool> {
            Ok(match (self.direction, &self.hi, &self.lo) {
                (Direction::Forward, Bound::Included(k), _) => {
                    pages.cmp_item(leaf, at, k)? != Ordering::Greater
                }
                (Direction::Forward, Bound::Excluded(k), _) => {
                    pages.cmp_item(leaf, at, k)? == Ordering::Less
                }
                (Direction::Backward, _, Bound::Included(k)) => {
                    pages.cmp_item(leaf, at, k)? != Ordering::Less
                }
                (Direction::Backward, _, Bound::Excluded(k)) => {
                    pages.cmp_item(leaf, at, k)? == Ordering::Greater
                }
                _ => true,
            })
        };
        let leaf_within = match self.leaf_within {
            Some(known) => known,
            None => {
                let edge = match self.direction {
                    Direction::Forward => leaf.count().saturating_sub(1),
                    Direction::Backward => 0,
                };
                compare(edge).unwrap_or(false)
            }
        };
        let within = leaf_within || compare(*i)?;
        let at = within.then(|| (leaf.clone(), *i));
        self.leaf_within = Some(leaf_within);
        Ok(at)
    }

    /// Moves to the next item within the bounds and returns its leaf and
    /// place, without reading its value.
    fn step(&mut self) -> Result<Option<(Arc<Page>, usize)>> {
        let moved = match self.state {
            State::Done => return Ok(None),
            State::Start => self.position(),
            State::Running => self.advance(),
        };
        self.state = State::Running;
        let at = match moved {
            Ok(true) => self.within(),
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        };
        if !matches!(at, Ok(Some(_))) {
            self.state = State::Done;
        }
        at
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
        let item = self.step().and_then(|at| {
            at.map(|(leaf, i)| {
                let pages = &self.tree.pages;
                let node = pages.item_node(&leaf, i)?;
                match leaf.item(i).key_part() {
                    (key, true) => Ok(f(key, node)),
                    (_, false) => Ok(f(&pages.item_key(&leaf, i)?, node)),
                }
            })
            .transpose()
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
        let item = self.step().and_then(|at| {
            at.map(|(leaf, i)| {
                let pages = &self.tree.pages;
                Ok(Item {
                    key: Key(pages.item_key(&leaf, i)?),
                    node: pages.item_node(&leaf, i)?,
                })
            })
            .transpose()
        });
        if item.is_err() {
            self.state = State::Done;
        }
        item.transpose()
    }
}

/// The children of one node: each child's segment and node.
pub struct Children<'t, 's, S: ?Sized> {
    tree: &'t Tree<'s, S>,
    prefix: Vec<u8>,
    end: Option<Vec<u8>>,
    bound: Bound<Vec<u8>>,
    direction: Direction,
    done: bool,
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
        let bound = self.bound.as_ref().map(Vec::as_slice);
        let mut cursor = if forward {
            self.tree.range(bound, upper, Direction::Forward)
        } else {
            self.tree.range(lower, bound, Direction::Backward)
        };
        // Only keys are read on the way: the node read is the child's own.
        let Some((leaf, i)) = cursor.step()? else {
            return Ok(None);
        };
        let orphan = || {
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
        let child_key = pages
            .key_through_segment(&leaf, i, self.prefix.len())?
            .ok_or_else(orphan)?;
        let child_key = child_key.as_slice();
        let child_end = child_key.len();
        let node = if child_end == item.key_len {
            pages.item_node(&leaf, i)?
        } else if forward {
            // The child sorts before everything under it.
            return Err(orphan());
        } else {
            self.tree.get_key(child_key)?.ok_or_else(orphan)?
        };
        let segment = key::decode(&child_key[self.prefix.len()..])
            .and_then(|mut s| s.pop())
            .unwrap_or_default();
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
    use crate::page::{Builder, EntryPages, MemStore, Numbering};
    use crate::tree::{check, Edit};

    /// Applies an entry in a unit of its own, committed.
    fn apply(store: &MemStore, root: Root, entry: u64, edits: &[Edit]) -> Result<Root> {
        let mut unit = store.begin()?;
        let new = crate::tree::apply(store, &mut unit, Some(root), entry, edits)?;
        unit.commit()?;
        Ok(new)
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
}
