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
        self.at(entry.number, entry.hash, entry.stamp, parent.level() - 1)
    }

    /// The page a parent names with `number`, `hash` and `stamp`, at
    /// `level`.
    pub(crate) fn at(
        &self,
        number: PageNo,
        hash: Hash,
        stamp: u64,
        level: u8,
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
    pub(crate) fn first_from(&self, page: &Page, lo: &[u8]) -> Result<Option<Vec<u8>>> {
        if page.kind() == PageKind::Leaf {
            let i = self.items_below(page, lo, false)?;
            return if i < page.count() {
                Ok(Some(self.item_key(page, i)?))
            } else {
                Ok(None)
            };
        }
        for c in self.separators_below(page, lo, true)?..page.count() {
            if let Some(key) = self.first_from(&*self.child(page, c)?, lo)? {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }

    /// The last key below `hi` (`None`: the last key) in the subtree of
    /// `page`.
    pub(crate) fn last_before(&self, page: &Page, hi: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
        if page.kind() == PageKind::Leaf {
            let n = match hi {
                Some(hi) => self.items_below(page, hi, false)?,
                None => page.count(),
            };
            return if n > 0 {
                Ok(Some(self.item_key(page, n - 1)?))
            } else {
                Ok(None)
            };
        }
        let last = match hi {
            Some(hi) => self.separators_below(page, hi, false)?,
            None => page.count() - 1,
        };
        for c in (0..=last).rev() {
            if let Some(key) = self.last_before(&*self.child(page, c)?, hi)? {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }

    /// The first `want` bytes of the part of a spilled stream that is not in
    /// its page: `rest` bytes in the overflow pages `refs`, written no later
    /// than `written`. Reads only the pages it needs.
    pub(crate) fn spill(
        &self,
        refs: Refs<'_>,
        rest: usize,
        want: usize,
        written: u64,
    ) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(want.min(rest));
        for (i, (number, hash)) in refs.iter().enumerate() {
            if out.len() >= want {
                break;
            }
            let page = self.read(number, hash)?;
            let expected = (rest - i * OVERFLOW_DATA).min(OVERFLOW_DATA);
            if page.kind() != PageKind::Overflow
                || page.data().len() != expected
                || page.written() > written
            {
                return Err(malformed(
                    &page,
                    "an overflow page that does not fit its item",
                ));
            }
            out.extend_from_slice(page.data());
        }
        out.truncate(want);
        Ok(out)
    }

    /// The whole key of item `i` of a leaf.
    pub(crate) fn item_key(&self, page: &Page, i: usize) -> Result<Vec<u8>> {
        let item = page.item(i);
        let (part, complete) = item.key_part();
        if complete {
            return Ok(part.to_vec());
        }
        let rest = item.key_len + item.value_len - SPILL_INLINE;
        let tail = self.spill(item.refs, rest, item.key_len - SPILL_INLINE, page.written())?;
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
                let tail = self.spill(item.refs, rest, rest, page.written())?;
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
        let Some(separator) = page.child(i).separator else {
            return Err(malformed(page, "a first child with a separator"));
        };
        let (part, complete) = separator.part();
        let mut out = part.to_vec();
        if !complete {
            let rest = separator.len - SPILL_INLINE;
            out.extend(self.spill(separator.refs, rest, rest, page.written())?);
        }
        Ok(out)
    }

    /// How item `i`'s key compares with `key`.
    pub(crate) fn cmp_item(&self, page: &Page, i: usize, key: &[u8]) -> Result<Ordering> {
        let (part, complete) = page.item(i).key_part();
        match quick_cmp(part, complete, key) {
            Some(order) => Ok(order),
            None => Ok(self.item_key(page, i)?.as_slice().cmp(key)),
        }
    }

    /// How the separator of child `i` compares with `key`.
    pub(crate) fn cmp_separator(&self, page: &Page, i: usize, key: &[u8]) -> Result<Ordering> {
        let Some(separator) = page.child(i).separator else {
            return Err(malformed(page, "a first child with a separator"));
        };
        let (part, complete) = separator.part();
        match quick_cmp(part, complete, key) {
            Some(order) => Ok(order),
            None => Ok(self.separator(page, i)?.as_slice().cmp(key)),
        }
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

/// Compares a key known as far as `part` with `key`, when that is enough.
fn quick_cmp(part: &[u8], complete: bool, key: &[u8]) -> Option<Ordering> {
    if complete {
        return Some(part.cmp(key));
    }
    let n = part.len().min(key.len());
    match part[..n].cmp(&key[..n]) {
        // `key` is a prefix of the known part, and the whole key is longer.
        Ordering::Equal if key.len() <= part.len() => Some(Ordering::Greater),
        Ordering::Equal => None,
        order => Some(order),
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
            state: State::Start,
        }
    }

    /// The children of the node at `path`, from the child with segment
    /// `from` (or the first or last child when unbounded) in `direction`.
    /// Each child is found by a seek, so a child's own subtree is skipped
    /// without being read.
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
    /// `entry`, where `page`'s own range is `[start, end)` (`None`:
    /// unbounded) and `page` was written after `entry`.
    fn changed_in(
        &self,
        page: &Page,
        lo: &[u8],
        hi: &[u8],
        start: Option<&[u8]>,
        end: Option<&[u8]>,
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
            if c > first && c < last {
                // Its range lies between two separators inside `[lo, hi)`.
                return Ok(true);
            }
            let child_start = if c == 0 {
                start.map(<[u8]>::to_vec)
            } else {
                Some(self.pages.separator(page, c)?)
            };
            let child_end = if c + 1 < page.count() {
                Some(self.pages.separator(page, c + 1)?)
            } else {
                end.map(<[u8]>::to_vec)
            };
            let inside = child_start.as_deref().is_some_and(|s| s >= lo)
                && child_end.as_deref().is_some_and(|e| e <= hi);
            if inside {
                return Ok(true);
            }
            if page.level() == 1 {
                // A leaf that holds part of the range and was written after
                // `entry`: counted as a change, unread (format section 2.8).
                return Ok(true);
            }
            let child = self.pages.child(page, c)?;
            if self.changed_in(
                &child,
                lo,
                hi,
                child_start.as_deref(),
                child_end.as_deref(),
                entry,
            )? {
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
    /// more of the item than the comparison with the bound needs.
    fn within(&self) -> Result<Option<(Arc<Page>, usize)>> {
        let Some((leaf, i)) = self.stack.last() else {
            return Ok(None);
        };
        let pages = &self.tree.pages;
        let within = match (self.direction, &self.hi, &self.lo) {
            (Direction::Forward, Bound::Included(k), _) => {
                pages.cmp_item(leaf, *i, k)? != Ordering::Greater
            }
            (Direction::Forward, Bound::Excluded(k), _) => {
                pages.cmp_item(leaf, *i, k)? == Ordering::Less
            }
            (Direction::Backward, _, Bound::Included(k)) => {
                pages.cmp_item(leaf, *i, k)? != Ordering::Less
            }
            (Direction::Backward, _, Bound::Excluded(k)) => {
                pages.cmp_item(leaf, *i, k)? == Ordering::Greater
            }
            _ => true,
        };
        Ok(within.then(|| (leaf.clone(), *i)))
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
        let (part, _) = item.key_part();
        // Where the child's segment ends. The page holds it unless the
        // child's own key is longer than the part of a spilled key a page
        // holds; only then is the found key read in full.
        let full;
        let (k, child_end) = match key::segment_end(part, self.prefix.len()) {
            Some(end) => (part, end),
            None => {
                full = pages.item_key(&leaf, i)?;
                let end = key::segment_end(&full, self.prefix.len()).ok_or_else(orphan)?;
                (full.as_slice(), end)
            }
        };
        let child_key = &k[..child_end];
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
