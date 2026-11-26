//! Building the next version of a tree from an entry's edits. The shape
//! rules here are tree format version 1 (`docs/server-storage-format.md`
//! section 2.7): change them only with a new version.
//!
//! An entry's edits are broken into primitives: set one item, change an
//! item that holds a value into one that holds both, delete a range of
//! keys. Each primitive is applied to a working copy of the pages it
//! reaches, and the copy is brought back within the shape rules before the
//! next primitive, so the shape after a primitive is a function of the
//! shape before it and the primitive alone. At the end of the entry every
//! page the entry changed is written once, bottom-up, stamped with the
//! entry's index.
//!
//! Items and separators copied from a page stay in that page: their keys
//! are compared with [`Pages`]' comparisons, which read overflow pages only
//! while the bytes so far are equal, and never read whole except where the
//! rules need the bytes (the boundary keys of a leaf split, and the first
//! and last keys a delete removes).

use std::cmp::Ordering;
use std::sync::Arc;

use super::key;
use super::read::{partition, KeyAt, Pages, Result};
use super::{check_edits, fault, Edit, Error, Node, Root};
use crate::page::{
    self, Builder, EntryPages, Fault, Hash, Page, PageKind, PageNo, PageStore, Release,
    CHILD_FOOTPRINT, KIND_BOTH, KIND_LIST, KIND_VALUE, MAX_LEVEL, OVERFLOW_DATA, SPILL_INLINE,
    USABLE,
};

/// A node of a page whose footprint shrank below this is joined with a
/// sibling.
const MIN_FILL: usize = USABLE / 4;
/// A node over capacity is split at its hint when what comes before the
/// hint fills at least this much of a page.
const SPLIT_HIGH: usize = USABLE * 7 / 8;

/// Applies the edits of entry `entry`, in order, to the version `root`
/// names (`None`: a tree the entry makes, which starts empty), and returns
/// the root of the new version. The old version's pages are left as they
/// were. An entry whose edits change an existing tree in nothing returns
/// `root` and writes nothing; a tree made is always written, if only as one
/// empty leaf.
///
/// Every edit is checked against the store's bounds before anything is
/// done ([`check_edits`]). `entry` must be above the index of the last entry
/// that changed the tree.
///
/// Pages are read through `store` and written into `out`, the entry's unit
/// of work, which `apply` does not commit: the caller names the new root
/// in the unit ([`EntryPages::set_root`], [`EntryPages::add_tree`]) and
/// commits the entry once. The unit sees only reads until every edit has
/// been applied to the working copy. An error leaves the unit with the
/// pages written so far; the caller drops it, and the store is as it was.
pub fn apply<S: PageStore + ?Sized, E: EntryPages>(
    store: &S,
    out: &mut E,
    root: Option<Root>,
    entry: u64,
    edits: &[Edit],
) -> Result<Root> {
    apply_with(store, out, root, entry, edits, true)
}

/// [`apply`], with the append path ([`append`]) or without it: the same
/// tree either way, which the tests check.
pub(crate) fn apply_with<S: PageStore + ?Sized, E: EntryPages>(
    store: &S,
    out: &mut E,
    root: Option<Root>,
    entry: u64,
    edits: &[Edit],
    appends: bool,
) -> Result<Root> {
    check_edits(edits)?;
    let pages = Pages { store };
    let mut tree = match root {
        Some(root) => {
            let top = pages.root(root)?;
            if entry <= top.written() {
                return Err(Error::EntryOrder {
                    entry,
                    last: top.written(),
                });
            }
            Work {
                slot: Slot::Clean(ChildRef {
                    page: root.page,
                    hash: root.hash,
                    stamp: top.written(),
                }),
                level: top.level(),
            }
        }
        None => Work {
            slot: Slot::Dirty(Box::new(WNode::Leaf(WLeaf::new(Vec::new())))),
            level: 0,
        },
    };
    let mut cx = Cx {
        pages,
        released: Vec::new(),
        changed: root.is_none(),
        last: if root.is_none() {
            Last::Empty
        } else {
            Last::Unknown { floor: None }
        },
        splits: 0,
    };
    // An entry of one put pays nothing for the append path.
    let appends = appends && edits.len() > 1;
    for edit in edits {
        match edit {
            Edit::Put { path, node } => {
                if appends && append(&mut cx, &mut tree, path, node)? {
                    #[cfg(test)]
                    APPENDED.with(|n| n.set(n.get() + 1));
                } else {
                    put(&mut cx, &mut tree, path, node)?;
                    if appends {
                        cx.last.forget(Some(key::encode(path)));
                    }
                }
            }
            Edit::Delete { path } => {
                let lo = key::encode(path);
                let hi = key::subtree_end(&lo);
                delete(&mut cx, &mut tree, &lo, hi.as_deref())?;
                if appends {
                    cx.last.forget(None);
                }
            }
        }
    }
    if let (false, Some(root)) = (cx.changed, root) {
        return Ok(root);
    }
    // Every entry that changes the tree rewrites its root.
    materialize(&mut cx, &mut tree.slot, tree.level, true)?;
    let new = write_slot(entry, out, tree.slot, tree.level)?;
    for (number, hash, how) in &cx.released {
        out.release(*number, hash, *how)?;
    }
    Ok(Root {
        page: new.page,
        hash: new.hash,
    })
}

#[cfg(test)]
thread_local! {
    /// The puts the append path made on this thread, for tests to check
    /// that the runs they rely on took it.
    static APPENDED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The greatest keys the append path proved on this thread.
    static PROVED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What the entry being applied carries along.
struct Cx<'s, S: ?Sized> {
    pages: Pages<'s, S>,
    /// Pages the new version no longer reaches, in the order found: a page
    /// before what it refers to.
    released: Vec<(PageNo, Hash, Release)>,
    changed: bool,
    /// What the append path knows of the working tree's greatest key.
    last: Last,
    /// The pages split so far, so that the append path knows when one of
    /// its puts split a page.
    splits: u64,
}

/// What the append path ([`append`]) knows of the working tree's greatest
/// key.
enum Last {
    /// The tree is empty: its root is a working leaf with no items.
    Empty,
    /// The greatest key, proven ([`proven_last`]): one this entry set,
    /// past which a lookup goes down the tree's right edge and finds
    /// nothing; its node's kind; and how many of its ancestors,
    /// shallowest first, are known to be lists (or both): looked up since
    /// the proof, or set by the append that made it the greatest.
    Proven {
        key: Vec<u8>,
        kind: u8,
        lists: usize,
    },
    /// Not proven: at the entry's start on a tree read from its pages,
    /// and after any edit the append path did not make. `floor` is a key
    /// the entry put or proved: a put at or below it is not tried, which
    /// is only cautious (the key may have gone since).
    Unknown { floor: Option<Vec<u8>> },
}

impl Last {
    /// After an edit the append path did not make, which put `put` (a key)
    /// or deleted: unproven, the floor raised to `put`.
    fn forget(&mut self, put: Option<Vec<u8>>) {
        let known = match std::mem::replace(self, Last::Unknown { floor: None }) {
            Last::Proven { key, .. } => Some(key),
            Last::Unknown { floor } => floor,
            Last::Empty => None,
        };
        *self = Last::Unknown {
            floor: known.max(put),
        };
    }
}

/// The working copy of the tree: its root and the root's level.
struct Work {
    slot: Slot,
    level: u8,
}

/// A child as its parent holds it.
#[derive(Clone, Copy)]
struct ChildRef {
    page: PageNo,
    hash: Hash,
    stamp: u64,
}

/// A child: a page not touched by the entry, or a copy being changed.
enum Slot {
    Clean(ChildRef),
    Dirty(Box<WNode>),
}

enum WNode {
    Leaf(WLeaf),
    Index(WIndex),
}

/// The footprint of a node is kept with it: summing it at each primitive
/// would cost as much as the rest of a primitive. This crate's own tests
/// check the total against the sum at each use; a debug build of a crate
/// that uses it does not, as summing there made a debug import of tens of
/// thousands of records several times slower.
struct WLeaf {
    items: Vec<WItem>,
    size: usize,
}

struct WIndex {
    level: u8,
    children: Vec<Slot>,
    /// `seps[k]` is the separator of `children[k + 1]`.
    seps: Vec<WSep>,
    size: usize,
}

struct WItem {
    kind: u8,
    src: Src,
}

/// Where an item's key and value are.
enum Src {
    /// Set by this entry.
    New { key: Vec<u8>, value: Vec<u8> },
    /// As item `.1` of page `.0` holds them, overflow pages included.
    Kept(Arc<Page>, usize),
}

/// A separator: made by this entry, or as child `.1` of page `.0` holds it,
/// overflow pages included.
enum WSep {
    New(Vec<u8>),
    Kept(Arc<Page>, usize),
}

impl WItem {
    fn new(key: Vec<u8>, kind: u8, value: Vec<u8>) -> WItem {
        WItem {
            kind,
            src: Src::New { key, value },
        }
    }

    fn key_len(&self) -> usize {
        match &self.src {
            Src::New { key, .. } => key.len(),
            Src::Kept(page, i) => page.item(*i).key_len,
        }
    }

    fn value_len(&self) -> usize {
        match &self.src {
            Src::New { value, .. } => value.len(),
            Src::Kept(page, i) => page.item(*i).value_len,
        }
    }

    fn footprint(&self) -> usize {
        page::item_footprint(self.key_len(), self.value_len())
    }

    /// How its key compares with `key`.
    fn cmp<S: PageStore + ?Sized>(&self, pages: &Pages<'_, S>, key: &[u8]) -> Result<Ordering> {
        match &self.src {
            Src::New { key: own, .. } => Ok(own.as_slice().cmp(key)),
            Src::Kept(page, i) => pages.cmp_item(page, *i, key),
        }
    }

    fn at(&self) -> KeyAt {
        match &self.src {
            Src::New { key, .. } => KeyAt::Bytes(key.clone()),
            Src::Kept(page, i) => KeyAt::Item(page.clone(), *i),
        }
    }

    /// Its key as far as memory holds it, and whether that is all of it.
    fn known_key(&self) -> (&[u8], bool) {
        match &self.src {
            Src::New { key, .. } => (key, true),
            Src::Kept(page, i) => page.item(*i).key_part(),
        }
    }
}

impl WSep {
    /// The separator as far as memory holds it, and whether that is all
    /// of it (nothing, for a kept child entry without one, which no page
    /// that parses holds).
    fn known(&self) -> (&[u8], bool) {
        match self {
            WSep::New(key) => (key, true),
            WSep::Kept(page, i) => page.child(*i).separator.map_or((&[], false), |s| s.part()),
        }
    }

    fn len(&self) -> usize {
        match self {
            WSep::New(key) => key.len(),
            WSep::Kept(page, i) => page.child(*i).separator.map_or(0, |s| s.len),
        }
    }

    fn footprint(&self) -> usize {
        page::separator_footprint(self.len())
    }

    fn cmp<S: PageStore + ?Sized>(&self, pages: &Pages<'_, S>, key: &[u8]) -> Result<Ordering> {
        match self {
            WSep::New(own) => Ok(own.as_slice().cmp(key)),
            WSep::Kept(page, i) => pages.cmp_separator(page, *i, key),
        }
    }
}

/// How many of `items` have a key below `key` (or equal to it).
fn items_below<S: PageStore + ?Sized>(
    pages: &Pages<'_, S>,
    items: &[WItem],
    key: &[u8],
    or_equal: bool,
) -> Result<usize> {
    partition(items.len(), |i| {
        let order = items[i].cmp(pages, key)?;
        Ok(order == Ordering::Less || (or_equal && order == Ordering::Equal))
    })
}

/// The item with key `key`, or where it would go.
fn find<S: PageStore + ?Sized>(
    pages: &Pages<'_, S>,
    items: &[WItem],
    key: &[u8],
) -> Result<std::result::Result<usize, usize>> {
    let i = items_below(pages, items, key, false)?;
    if i < items.len() && items[i].cmp(pages, key)? == Ordering::Equal {
        return Ok(Ok(i));
    }
    Ok(Err(i))
}

impl WLeaf {
    fn new(items: Vec<WItem>) -> WLeaf {
        let size = items.iter().map(WItem::footprint).sum();
        WLeaf { items, size }
    }

    fn footprint(&self) -> usize {
        #[cfg(test)]
        assert_eq!(
            self.size,
            self.items.iter().map(WItem::footprint).sum::<usize>()
        );
        self.size
    }
}

impl WIndex {
    fn new(level: u8, children: Vec<Slot>, seps: Vec<WSep>) -> WIndex {
        let mut ix = WIndex {
            level,
            children,
            seps,
            size: 0,
        };
        ix.recount();
        ix
    }

    /// Recounts the footprint after the children or separators changed.
    fn recount(&mut self) {
        self.size = self.children.len() * CHILD_FOOTPRINT
            + self.seps.iter().map(WSep::footprint).sum::<usize>();
    }

    fn footprint(&self) -> usize {
        #[cfg(test)]
        assert_eq!(
            self.size,
            self.children.len() * CHILD_FOOTPRINT
                + self.seps.iter().map(WSep::footprint).sum::<usize>()
        );
        self.size
    }

    /// How many separators are below `key` (or equal to it). With
    /// `or_equal`, that is the child whose range holds `key`; without, the
    /// last child whose range starts below `key`.
    fn seps_below<S: PageStore + ?Sized>(
        &self,
        pages: &Pages<'_, S>,
        key: &[u8],
        or_equal: bool,
    ) -> Result<usize> {
        partition(self.seps.len(), |t| {
            let order = self.seps[t].cmp(pages, key)?;
            Ok(order == Ordering::Less || (or_equal && order == Ordering::Equal))
        })
    }

    /// The child whose range holds `key`.
    fn child_for<S: PageStore + ?Sized>(&self, pages: &Pages<'_, S>, key: &[u8]) -> Result<usize> {
        if self.children.is_empty() {
            return Err(internal("an index node without children"));
        }
        self.seps_below(pages, key, true)
    }
}

impl WNode {
    fn footprint(&self) -> usize {
        match self {
            WNode::Leaf(leaf) => leaf.footprint(),
            WNode::Index(ix) => ix.footprint(),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            WNode::Leaf(leaf) => leaf.items.is_empty(),
            WNode::Index(ix) => ix.children.is_empty(),
        }
    }
}

fn internal(why: &'static str) -> Error {
    fault(0, Hash::default(), Fault::Malformed(why))
}

/// The page a clean slot names; `root` when the slot is the tree's root.
fn page_of<S: PageStore + ?Sized>(
    cx: &Cx<'_, S>,
    r: ChildRef,
    level: u8,
    root: bool,
) -> Result<Arc<Page>> {
    cx.pages.at(r.page, r.hash, r.stamp, level, !root)
}

/// The working node in `slot`, reading and copying its page first if the
/// entry has not touched it yet.
fn materialize<'a, S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    slot: &'a mut Slot,
    level: u8,
    root: bool,
) -> Result<&'a mut WNode> {
    if let Slot::Clean(r) = slot {
        let r = *r;
        let page = page_of(cx, r, level, root)?;
        let node = decode(&page)?;
        cx.released.push((r.page, r.hash, Release::Copied));
        *slot = Slot::Dirty(Box::new(node));
    }
    match slot {
        Slot::Dirty(node) => Ok(node),
        Slot::Clean(_) => Err(internal("a page that could not be copied")),
    }
}

/// The working copy of a page. Nothing beyond the page is read: items and
/// separators stay in it.
fn decode(page: &Arc<Page>) -> Result<WNode> {
    match page.kind() {
        PageKind::Leaf => {
            let items = (0..page.count())
                .map(|i| WItem {
                    kind: page.item(i).kind,
                    src: Src::Kept(page.clone(), i),
                })
                .collect();
            Ok(WNode::Leaf(WLeaf::new(items)))
        }
        PageKind::Index => {
            let mut children = Vec::with_capacity(page.count());
            let mut seps = Vec::with_capacity(page.count().saturating_sub(1));
            for i in 0..page.count() {
                let c = page.child(i);
                children.push(Slot::Clean(ChildRef {
                    page: c.number,
                    hash: c.hash,
                    stamp: c.stamp,
                }));
                if i > 0 {
                    seps.push(WSep::Kept(page.clone(), i));
                }
            }
            Ok(WNode::Index(WIndex::new(page.level(), children, seps)))
        }
        PageKind::Overflow => Err(fault(
            page.number(),
            page.hash(),
            Fault::Malformed("an overflow page in the tree"),
        )),
    }
}

/// The new version no longer reaches what this held.
fn drop_item<S: ?Sized>(cx: &mut Cx<'_, S>, item: &WItem) {
    if let Src::Kept(page, i) = &item.src {
        for (number, hash) in page.item(*i).refs.iter() {
            cx.released.push((number, hash, Release::Dropped));
        }
    }
}

fn drop_separator<S: ?Sized>(cx: &mut Cx<'_, S>, sep: &WSep) {
    if let WSep::Kept(page, i) = sep {
        if let Some(separator) = page.child(*i).separator {
            for (number, hash) in separator.refs.iter() {
                cx.released.push((number, hash, Release::Dropped));
            }
        }
    }
}

fn drop_slot<S: ?Sized>(cx: &mut Cx<'_, S>, slot: Slot) {
    match slot {
        Slot::Clean(r) => cx.released.push((r.page, r.hash, Release::Dropped)),
        Slot::Dirty(node) => match *node {
            WNode::Leaf(leaf) => {
                for item in &leaf.items {
                    drop_item(cx, item);
                }
            }
            WNode::Index(ix) => {
                for sep in &ix.seps {
                    drop_separator(cx, sep);
                }
                for child in ix.children {
                    drop_slot(cx, child);
                }
            }
        },
    }
}

// Reading the working tree. Untouched subtrees are read from their pages.

fn existing<S: PageStore + ?Sized>(
    cx: &Cx<'_, S>,
    slot: &Slot,
    level: u8,
    root: bool,
    key: &[u8],
) -> Result<Option<Node>> {
    match slot {
        Slot::Clean(r) => cx.pages.lookup(page_of(cx, *r, level, root)?, key),
        Slot::Dirty(node) => match &**node {
            WNode::Leaf(leaf) => match find(&cx.pages, &leaf.items, key)? {
                Ok(i) => Ok(Some(node_of(cx, &leaf.items[i])?)),
                Err(_) => Ok(None),
            },
            WNode::Index(ix) => {
                if ix.children.is_empty() {
                    return Ok(None);
                }
                let c = ix.child_for(&cx.pages, key)?;
                existing(cx, &ix.children[c], level - 1, false, key)
            }
        },
    }
}

fn node_of<S: PageStore + ?Sized>(cx: &Cx<'_, S>, item: &WItem) -> Result<Node> {
    let value = match &item.src {
        Src::New { value, .. } => value.clone(),
        Src::Kept(page, i) => cx.pages.item_node(page, *i)?.value().to_vec(),
    };
    Ok(Node::from_kind(item.kind, value))
}

fn first_from<S: PageStore + ?Sized>(
    cx: &Cx<'_, S>,
    slot: &Slot,
    level: u8,
    root: bool,
    lo: &[u8],
) -> Result<Option<KeyAt>> {
    match slot {
        Slot::Clean(r) => cx.pages.first_from(&page_of(cx, *r, level, root)?, lo),
        Slot::Dirty(node) => match &**node {
            WNode::Leaf(leaf) => {
                let i = items_below(&cx.pages, &leaf.items, lo, false)?;
                Ok(leaf.items.get(i).map(WItem::at))
            }
            WNode::Index(ix) => {
                if ix.children.is_empty() {
                    return Ok(None);
                }
                for child in &ix.children[ix.child_for(&cx.pages, lo)?..] {
                    if let Some(key) = first_from(cx, child, level - 1, false, lo)? {
                        return Ok(Some(key));
                    }
                }
                Ok(None)
            }
        },
    }
}

fn last_before<S: PageStore + ?Sized>(
    cx: &Cx<'_, S>,
    slot: &Slot,
    level: u8,
    root: bool,
    hi: Option<&[u8]>,
) -> Result<Option<KeyAt>> {
    match slot {
        Slot::Clean(r) => cx.pages.last_before(&page_of(cx, *r, level, root)?, hi),
        Slot::Dirty(node) => match &**node {
            WNode::Leaf(leaf) => {
                let n = match hi {
                    Some(hi) => items_below(&cx.pages, &leaf.items, hi, false)?,
                    None => leaf.items.len(),
                };
                Ok(n.checked_sub(1).map(|i| leaf.items[i].at()))
            }
            WNode::Index(ix) => {
                if ix.children.is_empty() {
                    return Ok(None);
                }
                let last = match hi {
                    Some(hi) => ix.seps_below(&cx.pages, hi, false)?,
                    None => ix.children.len() - 1,
                };
                for child in ix.children[..=last].iter().rev() {
                    if let Some(key) = last_before(cx, child, level - 1, false, hi)? {
                        return Ok(Some(key));
                    }
                }
                Ok(None)
            }
        },
    }
}

/// Whether the subtree of a child holds a key in `[lo, hi)`.
fn has_keys<S: PageStore + ?Sized>(
    cx: &Cx<'_, S>,
    slot: &Slot,
    level: u8,
    lo: &[u8],
    hi: Option<&[u8]>,
) -> Result<bool> {
    match first_from(cx, slot, level, false, lo)? {
        Some(key) => match hi {
            Some(hi) => Ok(cx.pages.cmp_at(&key, hi)? == Ordering::Less),
            None => Ok(true),
        },
        None => Ok(false),
    }
}

// The edits.

/// What the append path may take as the working tree's greatest key,
/// proven from the bytes the working tree holds and reading nothing:
/// [`Last::Empty`] when the root is a working leaf with no items; else,
/// when every node on the tree's right edge is a working copy (the entry
/// changed it), each one's separators or keys are in order as their bytes
/// show ([`page::known_in_order`]), each one's first separator or key is
/// at or above the separator its parent holds for it as the bytes show
/// (strictly above, for a separator), each last separator is at or below
/// the right leaf's last key as the bytes show, and that key is one this
/// entry set, that key. `above` is the parent's separator for the node at
/// `slot`, as far as memory holds it, and whether that is all of it.
///
/// Past that key, a lookup goes down the right edge, every separator there
/// being at or below it, and finds nothing at or after it, as the leaf's
/// keys are below it. An append that adds at the edge's end without
/// splitting a page keeps the edge so. One that splits a page makes new
/// separators from keys the proof saw only within their page, so it drops
/// the proof: the next put proves the greatest key again, and looks its
/// ancestors up again ([`append`]). Its ancestors are lists as the edit
/// that set it left them, unless something changed one since, which would
/// have removed the key; [`append`] looks them up once anyway, since a
/// tree read from its pages may hold keys a lookup does not find where
/// they are (reading a page checks its own bytes, not every key's parent
/// or the order across pages: `tree::check` does).
fn proven_last(slot: &Slot, root: bool, above: Option<(&[u8], bool)>) -> Option<Last> {
    let Slot::Dirty(node) = slot else {
        return None;
    };
    match &**node {
        WNode::Leaf(leaf) => {
            let Some(last) = leaf.items.last() else {
                return root.then_some(Last::Empty);
            };
            let Src::New { key, .. } = &last.src else {
                return None;
            };
            let ordered = leaf.items.windows(2).all(|pair| {
                let (a, whole) = pair[0].known_key();
                page::known_in_order(a, whole, pair[1].known_key().0)
            });
            let inside = above.is_none_or(|(sep, whole)| {
                page::known_at_most(sep, whole, leaf.items[0].known_key().0)
            });
            (ordered && inside).then(|| Last::Proven {
                key: key.clone(),
                kind: last.kind,
                lists: 0,
            })
        }
        WNode::Index(ix) => {
            let ordered = ix.seps.windows(2).all(|pair| {
                let (a, whole) = pair[0].known();
                page::known_in_order(a, whole, pair[1].known().0)
            });
            let inside = match (above, ix.seps.first()) {
                (Some((sep, whole)), Some(first)) => {
                    page::known_in_order(sep, whole, first.known().0)
                }
                _ => true,
            };
            if !(ordered && inside) {
                return None;
            }
            let below = ix.seps.last().map(WSep::known).or(above);
            let proven = proven_last(ix.children.last()?, false, below)?;
            let Last::Proven { key, .. } = &proven else {
                return None;
            };
            match ix.seps.last() {
                Some(sep) => {
                    let (s, whole) = sep.known();
                    page::known_at_most(s, whole, key).then_some(proven)
                }
                None => Some(proven),
            }
        }
    }
}

/// A put of a key greater than the working tree's greatest key, proven
/// ([`proven_last`]), as an import's are, applied with the primitives
/// [`put`] would apply and without the lookups that decide them; false,
/// with nothing done, for any other put, which [`put`] then makes. [`put`]
/// would look up each ancestor, delete the range under the key and look
/// the key up. Here:
///
/// - an ancestor at or below the greatest key is that key or one of its
///   ancestors (the greatest key lies between the ancestor and the put's
///   key, so under the ancestor). The greatest key's kind is known; one of
///   its ancestors is looked up as [`put`] looks it up, once after each
///   proof. When either is missing or holds only a value, [`put`] changes
///   it, and this declines;
/// - an ancestor above the greatest key is not there (a lookup of it goes
///   down the right edge and finds nothing), and is set as an empty list,
///   shallowest first, as [`put`] sets it;
/// - nothing is under the key and the key is not there, so the range
///   delete changes nothing (and [`put`]'s skips it) and the node is set.
///
/// So it applies the same primitives in the same order, and the tree is
/// the one [`put`] leaves, shape and hash included, on any tree.
///
/// It reads no page that [`put`] of the same key would not read: the
/// proof reads nothing, the ancestors' lookups are [`put`]'s own, and the
/// rest is the setting both do. [`put`] reads more: its ancestors' nodes
/// again for every key, and its lookups down the right edge, which the
/// proof holds in memory. So a page that fails every read fails an entry
/// through both paths or neither. A page that fails only some of its reads
/// can tell them apart, either way: [`put`] looks the shared ancestors up
/// again for every key, where this looks them up once after each proof;
/// and when this looks them up and then declines (an ancestor missing or
/// holding only a value), [`put`] looks them up again. A store whose
/// failures depend on how many reads came before can fail one path and
/// not the other.
fn append<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    tree: &mut Work,
    path: &[Vec<u8>],
    node: &Node,
) -> Result<bool> {
    let Some((_, ancestors)) = path.split_last() else {
        return Ok(false);
    };
    let key = key::encode(path);
    if let Last::Unknown { floor } = &cx.last {
        if floor.as_ref().is_some_and(|floor| key <= *floor) {
            return Ok(false);
        }
        match proven_last(&tree.slot, true, None) {
            Some(last) => {
                #[cfg(test)]
                PROVED.with(|n| n.set(n.get() + 1));
                cx.last = last;
            }
            None => return Ok(false),
        }
    }
    let (last, last_kind, mut lists) = match &cx.last {
        Last::Proven { key, kind, lists } => (Some(key.as_slice()), *kind, *lists),
        _ => (None, KIND_LIST, 0),
    };
    if last.is_some_and(|last| key.as_slice() <= last) {
        return Ok(false);
    }
    let mut ancestor = Vec::new();
    let mut missing = Vec::new();
    for (depth, segment) in (1..).zip(ancestors) {
        key::push_segment(&mut ancestor, segment);
        match last {
            Some(last) if ancestor.as_slice() < last => {
                if depth > lists {
                    match existing(cx, &tree.slot, tree.level, true, &ancestor)? {
                        Some(Node::List | Node::Both(_)) => lists = depth,
                        _ => return Ok(false),
                    }
                }
            }
            Some(last) if ancestor.as_slice() == last => {
                if last_kind == KIND_VALUE {
                    return Ok(false);
                }
            }
            _ => missing.push(ancestor.clone()),
        }
    }
    let splits = cx.splits;
    for ancestor in missing {
        set(
            cx,
            tree,
            Op::Set(WItem::new(ancestor, KIND_LIST, Vec::new())),
        )?;
    }
    let value = node.value().to_vec();
    set(
        cx,
        tree,
        Op::Set(WItem::new(key.clone(), node.kind(), value)),
    )?;
    cx.last = if cx.splits == splits {
        Last::Proven {
            key,
            kind: node.kind(),
            lists: usize::MAX,
        }
    } else {
        Last::Unknown { floor: Some(key) }
    };
    Ok(true)
}

fn put<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    tree: &mut Work,
    path: &[Vec<u8>],
    node: &Node,
) -> Result<()> {
    let Some((last, ancestors)) = path.split_last() else {
        return Err(internal("a put at the root"));
    };
    // Missing ancestors become empty lists; one that holds a value holds
    // both, but an empty value goes and leaves an empty list (as
    // `om_hbam::OwnedNode::ensure` does: a file holds a key as a value or
    // a list, and its writer writes a list's empty value as nothing).
    let mut key = Vec::new();
    for segment in ancestors {
        key::push_segment(&mut key, segment);
        match existing(cx, &tree.slot, tree.level, true, &key)? {
            None => set(
                cx,
                tree,
                Op::Set(WItem::new(key.clone(), KIND_LIST, Vec::new())),
            )?,
            Some(Node::Value(v)) if v.is_empty() => set(
                cx,
                tree,
                Op::Set(WItem::new(key.clone(), KIND_LIST, Vec::new())),
            )?,
            Some(Node::Value(_)) => set(cx, tree, Op::Both(key.clone()))?,
            Some(_) => {}
        }
    }
    key::push_segment(&mut key, last);
    // Everything under the node goes: keys above its own and below the end
    // of its subtree.
    let mut below = key.clone();
    below.push(0);
    let end = key::subtree_end(&key);
    delete(cx, tree, &below, end.as_deref())?;
    if existing(cx, &tree.slot, tree.level, true, &key)?.as_ref() != Some(node) {
        let value = node.value().to_vec();
        set(cx, tree, Op::Set(WItem::new(key, node.kind(), value)))?;
    }
    Ok(())
}

/// A change to one item.
enum Op {
    /// Insert or replace an item (always a new one).
    Set(WItem),
    /// The item with this key holds a value; make it hold both.
    Both(Vec<u8>),
}

impl Op {
    fn key(&self) -> &[u8] {
        match self {
            Op::Set(WItem {
                src: Src::New { key, .. },
                ..
            })
            | Op::Both(key) => key,
            Op::Set(_) => &[],
        }
    }
}

/// What a primitive did to a node, for its parent: the hint for a split
/// (format section 2.7), and whether the node's footprint shrank.
#[derive(Clone, Copy)]
struct Change {
    hint: Option<usize>,
    shrank: bool,
}

fn set<S: PageStore + ?Sized>(cx: &mut Cx<'_, S>, tree: &mut Work, op: Op) -> Result<()> {
    cx.changed = true;
    let change = set_in(cx, &mut tree.slot, tree.level, true, op)?;
    fix_root(cx, tree, change.hint)
}

fn set_in<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    slot: &mut Slot,
    level: u8,
    root: bool,
    op: Op,
) -> Result<Change> {
    let node = materialize(cx, slot, level, root)?;
    match node {
        WNode::Leaf(leaf) => {
            let found = find(&cx.pages, &leaf.items, op.key())?;
            let (hint, shrank) = match (op, found) {
                (Op::Set(item), Ok(i)) => {
                    let size = item.footprint();
                    let old = std::mem::replace(&mut leaf.items[i], item);
                    let was = old.footprint();
                    leaf.size = leaf.size - was + size;
                    drop_item(cx, &old);
                    (i, size < was)
                }
                (Op::Set(item), Err(i)) => {
                    leaf.size += item.footprint();
                    leaf.items.insert(i, item);
                    (i, false)
                }
                (Op::Both(_), Ok(i)) if leaf.items[i].kind == KIND_VALUE => {
                    leaf.items[i].kind = KIND_BOTH;
                    (i, false)
                }
                (Op::Both(_), _) => return Err(internal("no value to keep")),
            };
            Ok(Change {
                hint: Some(hint),
                shrank,
            })
        }
        WNode::Index(ix) => {
            let before = ix.footprint();
            let c = ix.child_for(&cx.pages, op.key())?;
            let change = set_in(cx, &mut ix.children[c], level - 1, false, op)?;
            let fixed = fix_child(cx, ix, c, change)?;
            Ok(Change {
                hint: fixed.split_hint(),
                shrank: ix.footprint() < before,
            })
        }
    }
}

/// Deletes every key in `[lo, hi)` (`hi` `None`: to the end). Returns
/// whether there was any.
fn delete<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    tree: &mut Work,
    lo: &[u8],
    hi: Option<&[u8]>,
) -> Result<bool> {
    let Some(first) = first_from(cx, &tree.slot, tree.level, true, lo)? else {
        return Ok(false);
    };
    if let Some(hi) = hi {
        if cx.pages.cmp_at(&first, hi)? != Ordering::Less {
            return Ok(false);
        }
    }
    let last = last_before(cx, &tree.slot, tree.level, true, hi)?.unwrap_or_else(|| first.clone());
    // The last step finds the leaves for these by their keys: the first and
    // last keys removed, read whole.
    let first = cx.pages.key_at(&first)?;
    let last = cx.pages.key_at(&last)?;
    cx.changed = true;
    delete_in(cx, &mut tree.slot, tree.level, true, lo, hi)?;
    fix_root(cx, tree, None)?;
    // The deleted keys' places now belong to the leaves around them, which
    // may be pages this primitive did not otherwise touch: they are
    // rewritten so that their stamps cover the deletion (format section
    // 2.8).
    touch(cx, &mut tree.slot, tree.level, true, &first)?;
    touch(cx, &mut tree.slot, tree.level, true, &last)?;
    Ok(true)
}

/// Deletes `[lo, hi)` from the subtree in `slot`, which holds keys in it.
fn delete_in<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    slot: &mut Slot,
    level: u8,
    root: bool,
    lo: &[u8],
    hi: Option<&[u8]>,
) -> Result<Change> {
    let node = materialize(cx, slot, level, root)?;
    match node {
        WNode::Leaf(leaf) => {
            let start = items_below(&cx.pages, &leaf.items, lo, false)?;
            let end = match hi {
                Some(hi) => items_below(&cx.pages, &leaf.items, hi, false)?,
                None => leaf.items.len(),
            };
            for item in leaf.items.drain(start..end.max(start)) {
                leaf.size -= item.footprint();
                drop_item(cx, &item);
            }
            Ok(Change {
                hint: None,
                shrank: end > start,
            })
        }
        WNode::Index(ix) => {
            let before = ix.footprint();
            let i = ix.child_for(&cx.pages, lo)?;
            let j = match hi {
                Some(hi) => ix.seps_below(&cx.pages, hi, false)?,
                None => ix.children.len() - 1,
            };
            // The children between the two ends go whole, unread; the left
            // end's range grows over theirs.
            if j > i + 1 {
                for sep in ix.seps.drain(i..j - 1) {
                    drop_separator(cx, &sep);
                }
                for child in ix.children.drain(i + 1..j).collect::<Vec<_>>() {
                    drop_slot(cx, child);
                }
                ix.recount();
            }
            let right = (j > i).then_some(i + 1);
            let mut changed = Vec::new();
            if has_keys(cx, &ix.children[i], level - 1, lo, hi)? {
                let change = delete_in(cx, &mut ix.children[i], level - 1, false, lo, hi)?;
                changed.push((i, change));
            }
            if let Some(r) = right {
                if has_keys(cx, &ix.children[r], level - 1, lo, hi)? {
                    let change = delete_in(cx, &mut ix.children[r], level - 1, false, lo, hi)?;
                    changed.push((r, change));
                }
            }
            // Fix the changed children left to right. Fixing the left one
            // can take in the right one.
            let mut done: Option<Fixed> = None;
            for (c, change) in changed {
                let c = match &done {
                    Some(f) if c >= f.start + f.old => c - f.old + f.new,
                    Some(f) if c >= f.start => continue,
                    _ => c,
                };
                done = Some(fix_child(cx, ix, c, change)?);
            }
            Ok(Change {
                hint: None,
                shrank: ix.footprint() < before,
            })
        }
    }
}

/// Copies every page on the way to the leaf whose range holds `key`.
fn touch<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    slot: &mut Slot,
    level: u8,
    root: bool,
    key: &[u8],
) -> Result<()> {
    let node = materialize(cx, slot, level, root)?;
    match node {
        WNode::Leaf(_) => Ok(()),
        WNode::Index(ix) => {
            if ix.children.is_empty() {
                return Ok(());
            }
            let c = ix.child_for(&cx.pages, key)?;
            touch(cx, &mut ix.children[c], level - 1, false, key)
        }
    }
}

// The shape rules.

/// How a fix-up changed a parent's children: `old` children from `start`
/// were replaced by `new` ones.
struct Fixed {
    start: usize,
    old: usize,
    new: usize,
}

impl Fixed {
    /// The hint for the parent (format section 2.7): when the fix split the
    /// changed child into parts, the place of the second part, which is the
    /// child's own place plus one; otherwise none.
    fn split_hint(&self) -> Option<usize> {
        (self.old == 1 && self.new > 1).then_some(self.start + 1)
    }
}

fn dirty(slot: &mut Slot) -> Result<&mut WNode> {
    match slot {
        Slot::Dirty(node) => Ok(node),
        Slot::Clean(_) => Err(internal("fixing a child that was not changed")),
    }
}

/// Brings child `c` of `ix`, which a primitive just changed, back within the
/// rules: split it if it is over capacity; join it with a sibling if it
/// shrank below a quarter of a page or is empty.
fn fix_child<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    ix: &mut WIndex,
    c: usize,
    change: Change,
) -> Result<Fixed> {
    let child = dirty(&mut ix.children[c])?;
    let footprint = child.footprint();
    if footprint > USABLE {
        let node = std::mem::replace(child, WNode::Leaf(WLeaf::new(Vec::new())));
        let parts = split(&cx.pages, node, change.hint)?;
        cx.splits += 1;
        let new = parts.len();
        replace(ix, c, 1, parts);
        return Ok(Fixed {
            start: c,
            old: 1,
            new,
        });
    }
    let empty = child.is_empty();
    if change.shrank && (empty || footprint < MIN_FILL) {
        if ix.children.len() > 1 {
            let mut fixed = join(cx, ix, c)?;
            // Two empty siblings join into an empty node, which does not
            // stay: it is joined again, or removed when it is alone.
            while fixed.new == 1 && dirty(&mut ix.children[fixed.start])?.is_empty() {
                if ix.children.len() == 1 {
                    ix.children.clear();
                    ix.recount();
                    fixed.new = 0;
                    break;
                }
                let again = join(cx, ix, fixed.start)?;
                fixed = Fixed {
                    start: again.start.min(fixed.start),
                    old: fixed.old + 1,
                    new: again.new,
                };
            }
            return Ok(fixed);
        }
        if empty {
            ix.children.clear();
            ix.recount();
            return Ok(Fixed {
                start: c,
                old: 1,
                new: 0,
            });
        }
    }
    Ok(Fixed {
        start: c,
        old: 1,
        new: 1,
    })
}

/// Replaces child `start` of `ix` (or nothing, `old` 0, in a new root) by
/// `parts`, each with the separator below it. The first part keeps the
/// separator of the child it replaces.
fn replace(ix: &mut WIndex, start: usize, old: usize, parts: Vec<(Option<WSep>, WNode)>) {
    debug_assert!(old <= 1);
    let mut children = Vec::with_capacity(parts.len());
    let mut seps = Vec::new();
    for (sep, node) in parts {
        if let Some(sep) = sep {
            seps.push(sep);
        }
        children.push(Slot::Dirty(Box::new(node)));
    }
    ix.children.splice(start..start + old, children);
    ix.seps.splice(start..start, seps);
    ix.recount();
}

/// Joins child `c` with its right sibling, or its left one if it is the
/// last child. The joined node is split again (at its middle) if it is over
/// capacity.
fn join<S: PageStore + ?Sized>(cx: &mut Cx<'_, S>, ix: &mut WIndex, c: usize) -> Result<Fixed> {
    let (l, r) = if c + 1 < ix.children.len() {
        (c, c + 1)
    } else {
        (c - 1, c)
    };
    let level = ix.level - 1;
    materialize(cx, &mut ix.children[l], level, false)?;
    materialize(cx, &mut ix.children[r], level, false)?;
    let sep = ix.seps.remove(r - 1);
    let Slot::Dirty(right) = ix.children.remove(r) else {
        return Err(internal("joining an unread child"));
    };
    ix.recount();
    let left = dirty(&mut ix.children[l])?;
    match (left, *right) {
        (WNode::Leaf(left), WNode::Leaf(right)) => {
            drop_separator(cx, &sep);
            left.size += right.size;
            left.items.extend(right.items);
        }
        (WNode::Index(left), WNode::Index(right)) => {
            if left.children.is_empty() || right.children.is_empty() {
                drop_separator(cx, &sep);
                if left.children.is_empty() {
                    *left = right;
                }
            } else {
                left.seps.push(sep);
                left.seps.extend(right.seps);
                left.children.extend(right.children);
                left.recount();
            }
        }
        _ => return Err(internal("siblings at different levels")),
    }
    let joined = dirty(&mut ix.children[l])?;
    if joined.footprint() > USABLE {
        let node = std::mem::replace(joined, WNode::Leaf(WLeaf::new(Vec::new())));
        let parts = split(&cx.pages, node, None)?;
        let new = parts.len();
        replace(ix, l, 1, parts);
        return Ok(Fixed {
            start: l,
            old: 2,
            new,
        });
    }
    Ok(Fixed {
        start: l,
        old: 2,
        new: 1,
    })
}

/// Splits a node over capacity into parts that fit, each but the first with
/// the separator below it.
fn split<S: PageStore + ?Sized>(
    pages: &Pages<'_, S>,
    node: WNode,
    hint: Option<usize>,
) -> Result<Vec<(Option<WSep>, WNode)>> {
    let mut parts = Vec::new();
    split_into(pages, node, hint, None, &mut parts)?;
    Ok(parts)
}

fn split_into<S: PageStore + ?Sized>(
    pages: &Pages<'_, S>,
    node: WNode,
    hint: Option<usize>,
    sep: Option<WSep>,
    parts: &mut Vec<(Option<WSep>, WNode)>,
) -> Result<()> {
    if node.footprint() <= USABLE {
        parts.push((sep, node));
        return Ok(());
    }
    match node {
        WNode::Leaf(WLeaf { mut items, .. }) => {
            let sizes: Vec<usize> = items.iter().map(WItem::footprint).collect();
            let mut prefix = vec![0usize; sizes.len() + 1];
            for (i, size) in sizes.iter().enumerate() {
                prefix[i + 1] = prefix[i] + size;
            }
            let total = prefix[sizes.len()];
            let s = choose(sizes.len(), hint, |s| prefix[s], |s| total - prefix[s])?;
            let right = items.split_off(s);
            // The separator needs the two keys at the boundary.
            let left_last = pages.key_at(&items[s - 1].at())?;
            let right_first = pages.key_at(&right[0].at())?;
            let up = WSep::New(shortest_separator(&left_last, &right_first));
            split_into(pages, WNode::Leaf(WLeaf::new(items)), None, sep, parts)?;
            split_into(pages, WNode::Leaf(WLeaf::new(right)), None, Some(up), parts)
        }
        WNode::Index(mut ix) => {
            let n = ix.children.len();
            // prefix[t]: the footprint of the separators of children 0..t
            // (child 0 has none).
            let mut prefix = vec![0usize; n + 1];
            for t in 0..n {
                let sep = if t == 0 {
                    0
                } else {
                    ix.seps[t - 1].footprint()
                };
                prefix[t + 1] = prefix[t] + sep;
            }
            // Left of a split at `s`: children 0..s and their separators.
            // Right: children s..n; child s's separator moves up.
            let left = |s: usize| s * CHILD_FOOTPRINT + prefix[s];
            let right = |s: usize| (n - s) * CHILD_FOOTPRINT + (prefix[n] - prefix[s + 1]);
            let s = choose(n, hint, left, right)?;
            let right_children = ix.children.split_off(s);
            let mut right_seps = ix.seps.split_off(s - 1);
            let up = right_seps.remove(0);
            let level = ix.level;
            ix.recount();
            split_into(pages, WNode::Index(ix), None, sep, parts)?;
            split_into(
                pages,
                WNode::Index(WIndex::new(level, right_children, right_seps)),
                None,
                Some(up),
                parts,
            )
        }
    }
}

/// Where to split `n` units, given the footprints of the two sides of a
/// split before unit `s`. At the hint when what precedes it fills at least
/// [`SPLIT_HIGH`] and both sides fit (an append fills its page); otherwise
/// where the two sides are nearest equal, the first such place on a tie.
fn choose(
    n: usize,
    hint: Option<usize>,
    left: impl Fn(usize) -> usize,
    right: impl Fn(usize) -> usize,
) -> Result<usize> {
    if n < 2 {
        return Err(internal("splitting a single unit"));
    }
    if let Some(h) = hint {
        if (1..n).contains(&h) && left(h) >= SPLIT_HIGH && left(h) <= USABLE && right(h) <= USABLE {
            return Ok(h);
        }
    }
    let mut best = 1;
    let mut best_gap = usize::MAX;
    for s in 1..n {
        let gap = left(s).abs_diff(right(s));
        if gap < best_gap {
            best = s;
            best_gap = gap;
        }
    }
    Ok(best)
}

/// The shortest key above `left` and at or below `right` (`left < right`):
/// `right` cut just after the first byte where they differ, or just after
/// the byte that follows `left` when `left` is a proper prefix of `right`.
fn shortest_separator(left: &[u8], right: &[u8]) -> Vec<u8> {
    let common = left.iter().zip(right).take_while(|(a, b)| a == b).count();
    right[..(common + 1).min(right.len())].to_vec()
}

/// After a primitive: a root over capacity is split under a new root; an
/// index root with one child is replaced by that child, and one with none
/// by an empty leaf.
fn fix_root<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    tree: &mut Work,
    hint: Option<usize>,
) -> Result<()> {
    loop {
        let node = match &mut tree.slot {
            Slot::Dirty(node) => node,
            // A root the entry has not copied gives way to its only child
            // too: read it, and promote the child without copying either.
            Slot::Clean(r) => {
                if tree.level == 0 {
                    return Ok(());
                }
                let r = *r;
                let page = page_of(cx, r, tree.level, true)?;
                if page.count() != 1 {
                    return Ok(());
                }
                let child = page.child(0);
                cx.released.push((r.page, r.hash, Release::Copied));
                tree.slot = Slot::Clean(ChildRef {
                    page: child.number,
                    hash: child.hash,
                    stamp: child.stamp,
                });
                tree.level -= 1;
                continue;
            }
        };
        if node.footprint() > USABLE {
            if tree.level >= MAX_LEVEL {
                return Err(Error::TooTall);
            }
            let node = std::mem::replace(&mut **node, WNode::Leaf(WLeaf::new(Vec::new())));
            let parts = split(&cx.pages, node, hint)?;
            cx.splits += 1;
            let mut ix = WIndex::new(tree.level + 1, Vec::new(), Vec::new());
            replace(&mut ix, 0, 0, parts);
            tree.slot = Slot::Dirty(Box::new(WNode::Index(ix)));
            tree.level += 1;
            continue;
        }
        match &mut **node {
            WNode::Index(ix) if ix.children.is_empty() => {
                tree.slot = Slot::Dirty(Box::new(WNode::Leaf(WLeaf::new(Vec::new()))));
                tree.level = 0;
            }
            WNode::Index(ix) if ix.children.len() == 1 => {
                let child = ix.children.remove(0);
                tree.slot = child;
                tree.level -= 1;
                continue;
            }
            _ => {}
        }
        return Ok(());
    }
}

// Writing the new pages.

fn write_page<E: EntryPages>(out: &mut E, builder: Builder) -> Result<ChildRef> {
    let number = out.allocate()?;
    let page = builder
        .finish(number)
        .map_err(|f| fault(number, Hash::default(), f))?;
    let r = ChildRef {
        page: number,
        hash: page.hash(),
        stamp: page.written(),
    };
    out.write(Arc::new(page))?;
    Ok(r)
}

/// Writes `stream[SPILL_INLINE..]` to new overflow pages.
fn write_overflow<E: EntryPages>(
    entry: u64,
    out: &mut E,
    stream: &[u8],
) -> Result<Vec<(PageNo, Hash)>> {
    let mut refs = Vec::new();
    for chunk in stream[SPILL_INLINE..].chunks(OVERFLOW_DATA) {
        let mut builder = Builder::new(PageKind::Overflow, 0, entry);
        builder.data(chunk);
        let r = write_page(out, builder)?;
        refs.push((r.page, r.hash));
    }
    Ok(refs)
}

/// Writes the working node in `slot`, children first, every page stamped
/// with entry `entry`.
fn write_slot<E: EntryPages>(entry: u64, out: &mut E, slot: Slot, level: u8) -> Result<ChildRef> {
    let node = match slot {
        Slot::Clean(r) => return Ok(r),
        Slot::Dirty(node) => *node,
    };
    match node {
        WNode::Leaf(leaf) => {
            let mut builder = Builder::new(PageKind::Leaf, 0, entry);
            for item in &leaf.items {
                match &item.src {
                    Src::New { key, value } => {
                        let mut stream = Vec::with_capacity(key.len() + value.len());
                        stream.extend_from_slice(key);
                        stream.extend_from_slice(value);
                        match page::item_inline(key.len(), value.len()) {
                            Some(inline) => {
                                let refs = write_overflow(entry, out, &stream)?;
                                builder.item(
                                    item.kind,
                                    key.len(),
                                    value.len(),
                                    &stream[..inline],
                                    &refs,
                                );
                            }
                            None => builder.item(item.kind, key.len(), value.len(), &stream, &[]),
                        }
                    }
                    Src::Kept(page, i) => {
                        let kept = page.item(*i);
                        let refs: Vec<_> = kept.refs.iter().collect();
                        builder.item(item.kind, kept.key_len, kept.value_len, kept.inline, &refs);
                    }
                }
            }
            write_page(out, builder)
        }
        WNode::Index(ix) => {
            let mut children = Vec::with_capacity(ix.children.len());
            for child in ix.children {
                children.push(write_slot(entry, out, child, level - 1)?);
            }
            let mut builder = Builder::new(PageKind::Index, level, entry);
            for (t, child) in children.iter().enumerate() {
                if t == 0 {
                    builder.child(child.page, &child.hash, child.stamp, None);
                    continue;
                }
                match &ix.seps[t - 1] {
                    WSep::Kept(page, i) => {
                        let Some(kept) = page.child(*i).separator else {
                            return Err(internal("a kept separator that is not there"));
                        };
                        let refs: Vec<_> = kept.refs.iter().collect();
                        builder.child(
                            child.page,
                            &child.hash,
                            child.stamp,
                            Some((kept.len, kept.inline, &refs)),
                        );
                    }
                    WSep::New(key) => match page::separator_inline(key.len()) {
                        Some(inline) => {
                            let refs = write_overflow(entry, out, key)?;
                            builder.child(
                                child.page,
                                &child.hash,
                                child.stamp,
                                Some((key.len(), &key[..inline], &refs)),
                            );
                        }
                        None => builder.child(
                            child.page,
                            &child.hash,
                            child.stamp,
                            Some((key.len(), key, &[])),
                        ),
                    },
                }
            }
            write_page(out, builder)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod appends {
        //! The append path ([`super::append`]) leaves the tree the edits
        //! leave without it, shape and hash included, after every entry: for
        //! ordered inputs, for the inputs on which it must decline, for
        //! inputs at the store's limits, and on trees the writer never
        //! makes, in splits of the edits into entries, on `MemStore` and on
        //! the page file.

        use super::super::{apply_with, APPENDED, PROVED};
        use crate::page::{
            EntryPages, Hash, MemStore, Numbering, PageStore, Root, TreeName, KIND_VALUE, MAX_KEY,
            MAX_VALUE,
        };
        use crate::pagefile::{Config, EntryId, PageFile, Purpose};
        use crate::sim::Sim;
        use crate::sys::Fs;
        use crate::tree::{Edit, Node};

        struct Rng(u64);

        impl Rng {
            fn next(&mut self) -> u64 {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.0 = x;
                x
            }

            fn below(&mut self, n: usize) -> usize {
                (self.next() % n.max(1) as u64) as usize
            }
        }

        /// A random key tree's nodes, parents first and siblings in key
        /// order, as an import puts them: lists, values (some empty, some
        /// long enough to spill) and nodes that are both; some a value
        /// with children put under it, which a put turns into both or a
        /// list.
        fn ordered(rng: &mut Rng, size: usize) -> Vec<Edit> {
            fn node(rng: &mut Rng) -> Node {
                let kind = rng.below(5);
                let value = match rng.below(6) {
                    0 => Vec::new(),
                    1 => vec![9; 2000 + rng.below(3000)],
                    n => vec![n as u8; rng.below(40)],
                };
                match kind {
                    0 | 1 => Node::List,
                    2 => Node::Both(value),
                    _ => Node::Value(value),
                }
            }
            fn children(
                rng: &mut Rng,
                path: &mut Vec<Vec<u8>>,
                out: &mut Vec<Edit>,
                budget: &mut usize,
            ) {
                let mut segments: Vec<Vec<u8>> = (0..1 + rng.below(6))
                    .map(|_| {
                        (0..1 + rng.below(3))
                            .map(|_| [0, 1, 0xff, 7][rng.below(4)])
                            .collect()
                    })
                    .collect();
                segments.sort();
                segments.dedup();
                for segment in segments {
                    if *budget == 0 {
                        return;
                    }
                    *budget -= 1;
                    path.push(segment);
                    let node = node(rng);
                    out.push(Edit::Put {
                        path: path.clone(),
                        node,
                    });
                    if path.len() < 6 && rng.below(3) > 0 {
                        children(rng, path, out, budget);
                    }
                    path.pop();
                }
            }
            // Top-level keys numbered in order, each with a subtree below.
            let mut out = Vec::new();
            let mut budget = size;
            let mut top = 0u16;
            while budget > 0 {
                budget -= 1;
                let mut path = vec![top.to_be_bytes().to_vec()];
                top += 1;
                let node = node(rng);
                out.push(Edit::Put {
                    path: path.clone(),
                    node,
                });
                children(rng, &mut path, &mut out, &mut budget);
            }
            out
        }

        /// `edits` with some disorder: a few puts moved earlier, replaced,
        /// or deleted, so that the append path meets what it declines.
        fn disordered(rng: &mut Rng, mut edits: Vec<Edit>) -> Vec<Edit> {
            let n = edits.len();
            for _ in 0..n / 10 {
                let i = rng.below(n);
                let j = rng.below(n);
                match rng.below(4) {
                    0 => edits.swap(i, j),
                    1 => {
                        let again = edits[i].clone();
                        edits.insert(j, again);
                    }
                    2 => {
                        if let Edit::Put { path, .. } = &edits[i] {
                            let delete = Edit::delete(path.clone());
                            edits.insert(j, delete);
                        }
                    }
                    _ => {
                        // An ancestor of the next key put again just before
                        // it: its subtree so far goes, the greatest key
                        // with it, and the next keys come below it.
                        if let Edit::Put { path, .. } = &edits[j] {
                            if path.len() > 1 {
                                let ancestor = path[..1 + rng.below(path.len() - 1)].to_vec();
                                edits.insert(j, Edit::list(ancestor));
                            }
                        }
                    }
                }
            }
            edits
        }

        /// The roots of the tree `edits` make in `MemStore`, after each entry
        /// as `cuts` split them.
        fn on_memory(edits: &[Edit], cuts: &[usize], appends: bool) -> Vec<Hash> {
            let store = MemStore::new(Numbering::Sequential);
            let roots = from_memory(&store, None, edits, cuts, appends);
            roots.iter().map(|root| root.hash).collect()
        }

        /// The same from `root` in `store`, written by entry 1 if any (an
        /// entry's number is in the pages it writes, so in their hashes).
        fn from_memory(
            store: &MemStore,
            mut root: Option<Root>,
            edits: &[Edit],
            cuts: &[usize],
            appends: bool,
        ) -> Vec<Root> {
            let first = if root.is_some() { 2 } else { 1 };
            let mut roots = Vec::new();
            for (entry, batch) in (first..).zip(batches(edits, cuts)) {
                let mut unit = store.begin().unwrap();
                let new = apply_with(store, &mut unit, root, entry, batch, appends).unwrap();
                unit.commit().unwrap();
                roots.push(new);
                root = Some(new);
            }
            roots
        }

        /// The same on the page file on the simulated disk, with the append
        /// path: each entry reserved for, and checkpointed when the pages
        /// waiting for one reach the budget.
        fn on_page_file(edits: &[Edit], cuts: &[usize]) -> Vec<Hash> {
            let sim = Sim::new(5);
            let fs = sim.fs();
            let lock = fs.lock().unwrap();
            let config = Config {
                cache_bytes: 64 << 10,
                dirty_budget: 16,
                reserve: 16,
                growth: 64,
            };
            let pf = PageFile::create(&fs, &lock, config).unwrap();
            let name = TreeName::new(b"t".to_vec()).unwrap();
            let mut root = None;
            let mut roots = Vec::new();
            for (entry, batch) in (1..).zip(batches(edits, cuts)) {
                let mut pages = 64;
                let new = loop {
                    let res = match pf.reserve(entry, pages, Purpose::Entry) {
                        Ok(res) => res,
                        Err(crate::pagefile::SpaceError::CheckpointNeeded { .. }) => {
                            let id = EntryId {
                                index: pf.newest_entry(),
                                term: 0,
                                hash: crate::page::Hash::default(),
                            };
                            pf.checkpoint(id, None).unwrap();
                            continue;
                        }
                        Err(e) => panic!("{e}"),
                    };
                    let mut unit = res.begin().unwrap();
                    match apply_with(&res, &mut unit, root, entry, batch, true) {
                        Ok(new) => {
                            match root {
                                None => unit.add_tree(&name, new),
                                Some(old) if old != new => unit.set_root(&name, new),
                                Some(_) => {}
                            }
                            unit.commit().unwrap();
                            break new;
                        }
                        Err(crate::tree::Error::Store(crate::page::StoreError::Full)) => {
                            pages *= 4;
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                roots.push(new.hash);
                root = Some(new);
            }
            roots
        }

        fn batches<'e>(edits: &'e [Edit], cuts: &[usize]) -> Vec<&'e [Edit]> {
            let mut out = Vec::new();
            let mut start = 0;
            for &cut in cuts.iter().chain(std::iter::once(&edits.len())) {
                if cut > start {
                    out.push(&edits[start..cut]);
                    start = cut;
                }
            }
            out
        }

        /// The puts the append path makes while `f` runs, on this thread.
        fn appended<T>(f: impl FnOnce() -> T) -> (T, usize) {
            let before = APPENDED.with(|n| n.get());
            let out = f();
            (out, APPENDED.with(|n| n.get()) - before)
        }

        /// The greatest keys the append path proves while `f` runs.
        fn proved<T>(f: impl FnOnce() -> T) -> (T, usize) {
            let before = PROVED.with(|n| n.get());
            let out = f();
            (out, PROVED.with(|n| n.get()) - before)
        }

        /// The root page's level after `edits` in one entry.
        fn height(edits: &[Edit]) -> u8 {
            let store = MemStore::new(Numbering::Sequential);
            let mut unit = store.begin().unwrap();
            let root = apply_with(&store, &mut unit, None, 1, edits, true).unwrap();
            unit.commit().unwrap();
            store.read(root.page, &root.hash).unwrap().level()
        }

        /// Every split of short sequences, ordered and not, into entries,
        /// in memory: the append path's tree is the other's, root hash for
        /// root hash after every entry.
        #[test]
        fn every_split_of_short_sequences() {
            let mut splits = 0;
            let mut appends = 0;
            for seed in 1..=12u64 {
                let mut rng = Rng(seed * 0x2545_f491);
                let ordered = ordered(&mut rng, 11);
                let edits = if seed % 3 == 0 {
                    disordered(&mut rng, ordered)
                } else {
                    ordered
                };
                let n = edits.len();
                for mask in 0..1u32 << (n - 1).min(12) {
                    let cuts: Vec<usize> = (1..n).filter(|i| mask & (1 << (i - 1)) != 0).collect();
                    let (got, made) = appended(|| on_memory(&edits, &cuts, true));
                    assert_eq!(
                        got,
                        on_memory(&edits, &cuts, false),
                        "seed {seed}, cuts {cuts:?}"
                    );
                    splits += 1;
                    appends += made;
                }
            }
            assert!(splits > 10_000, "{splits}");
            assert!(appends > splits, "{appends} appends in {splits} splits");
        }

        /// Long sequences, deep enough for splits and index levels, in
        /// random splits: the same on `MemStore` and on the page file.
        #[test]
        fn random_splits_of_long_sequences_on_memory_and_the_page_file() {
            for seed in 1..=6u64 {
                let mut rng = Rng(seed * 0x9e37_79b9);
                let ordered = ordered(&mut rng, 3_000);
                let edits = if seed % 2 == 0 {
                    disordered(&mut rng, ordered)
                } else {
                    ordered
                };
                for _ in 0..4 {
                    let mut cuts: Vec<usize> =
                        (0..rng.below(20)).map(|_| rng.below(edits.len())).collect();
                    cuts.sort();
                    let expected = on_memory(&edits, &cuts, false);
                    let (got, made) = appended(|| on_memory(&edits, &cuts, true));
                    assert_eq!(got, expected, "seed {seed}");
                    // A put moved early makes the greatest key one that
                    // the puts after it do not pass for a while.
                    let least = if seed % 2 == 0 { 1 } else { edits.len() / 2 };
                    assert!(made >= least, "seed {seed}: {made} appends");
                    assert_eq!(on_page_file(&edits, &cuts), expected, "seed {seed}");
                }
            }
        }

        fn put(path: &[&[u8]], node: Node) -> Edit {
            Edit::Put {
                path: path.iter().map(|s| s.to_vec()).collect(),
                node,
            }
        }

        /// Inputs the generators do not reach, in splits that put each
        /// event inside an entry and at its edges: keys that spill, a key
        /// and a value at the store's limits, a path at the depth limit, a
        /// tree tall enough for a split at every index level (its root
        /// growing three times), the greatest key deleted, and a tree
        /// emptied and filled again. Each happens, and the append path
        /// makes puts in each.
        #[test]
        fn directed_inputs_at_the_limits() {
            let mut sequences: Vec<(&str, Vec<Edit>)> = Vec::new();
            // Keys of 1,505 bytes with values of 3,000: their items spill,
            // keys included, so that only their first 1,024 bytes are in
            // their leaves.
            sequences.push((
                "spilled keys",
                (0..40u32)
                    .flat_map(|i| {
                        let top = [&[b'k'; 1500][..], &i.to_be_bytes()].concat();
                        [
                            put(&[&top], Node::List),
                            put(&[&top, b"v"], Node::Value(vec![1; 3000])),
                        ]
                    })
                    .collect(),
            ));
            // A key of exactly MAX_KEY bytes and a value of MAX_VALUE, after
            // keys below them.
            let first = vec![0xfe; 32_000];
            let second = vec![0x41; MAX_KEY - 4 - first.len()];
            assert_eq!(crate::tree::key::encoded_len(&[&first, &second]), MAX_KEY);
            sequences.push((
                "limits",
                vec![
                    put(&[b"a"], Node::Value(vec![2; MAX_VALUE])),
                    put(&[b"b"], Node::List),
                    put(&[&first, &second], Node::Value(vec![3; MAX_VALUE])),
                    put(&[&[0xff]], Node::Value(vec![4; 10])),
                ],
            ));
            // A path of MAX_DEPTH segments, every ancestor missing.
            let deep: Vec<Vec<u8>> = (0..crate::tree::MAX_DEPTH as u8)
                .map(|d| vec![d + 1])
                .collect();
            sequences.push((
                "depth",
                vec![
                    put(&[b"\x01"], Node::List),
                    Edit::Put {
                        path: deep.clone(),
                        node: Node::Value(vec![5]),
                    },
                    put(&[&[0x7f]], Node::List),
                ],
            ));
            // Keys sharing their first 3,900 bytes, so that separators are
            // long and index pages hold few: a tall tree.
            let tall: Vec<Edit> = (0..400u32)
                .map(|i| {
                    let top = [&[b't'; 3900][..], &(i + 0x0101_0101).to_be_bytes()].concat();
                    put(&[&top], Node::Value(vec![6; 8]))
                })
                .collect();
            assert!(height(&tall) >= 3, "the tall tree's height");
            sequences.push(("tall", tall));
            // The greatest key deleted, then keys past it; the whole tree
            // deleted, then filled again.
            let numbered = |i: u8| put(&[&[b'n', i]], Node::Value(vec![i]));
            let mut deletes: Vec<Edit> = (1..10).map(numbered).collect();
            deletes.push(Edit::delete(vec![vec![b'n', 9]]));
            deletes.extend((10..20).map(numbered));
            deletes.push(Edit::delete(Vec::new()));
            deletes.extend((20..30).map(numbered));
            sequences.push(("deletes", deletes));

            let mut rng = Rng(0x5eed);
            for (name, edits) in &sequences {
                let n = edits.len();
                let mut splits: Vec<Vec<usize>> = vec![
                    Vec::new(),
                    (1..n).step_by(2).collect(),
                    (1..n).step_by(5).collect(),
                ];
                for _ in 0..3 {
                    let mut cuts: Vec<usize> = (0..1 + rng.below(4))
                        .map(|_| 1 + rng.below(n - 1))
                        .collect();
                    cuts.sort();
                    splits.push(cuts);
                }
                for cuts in splits {
                    let expected = on_memory(edits, &cuts, false);
                    let (got, made) = appended(|| on_memory(edits, &cuts, true));
                    assert_eq!(got, expected, "{name}, cuts {cuts:?}");
                    if cuts.is_empty() {
                        assert!(made > 0, "{name}: no put took the append path");
                    }
                    assert_eq!(
                        on_page_file(edits, &cuts),
                        expected,
                        "{name}, cuts {cuts:?}"
                    );
                }
            }
        }

        /// The runs [`directed_inputs_at_the_limits`] relies on, counted in
        /// one entry: after the greatest key is deleted the next put proves
        /// the greatest key again and appends, and after the whole tree is
        /// deleted the next put appends to the empty tree.
        #[test]
        fn the_append_path_resumes_after_deletes() {
            let numbered = |i: u8| put(&[&[b'n', i]], Node::Value(vec![i]));
            let one = |edits: &[Edit]| {
                let (got, made) = appended(|| on_memory(edits, &[], true));
                assert_eq!(got, on_memory(edits, &[], false));
                made
            };
            // Three appends; the greatest deleted; two appends after it.
            let greatest = vec![
                numbered(1),
                numbered(2),
                numbered(3),
                Edit::delete(vec![vec![b'n', 3]]),
                numbered(4),
                numbered(5),
            ];
            assert_eq!(one(&greatest), 5);
            // Two appends; the tree emptied; two appends into it.
            let emptied = vec![
                numbered(1),
                numbered(2),
                Edit::delete(Vec::new()),
                numbered(3),
                numbered(4),
            ];
            assert_eq!(one(&emptied), 4);
        }

        /// Review of #769, rule 2: the append path reads nothing to decide,
        /// so a page that fails every read and holds only the greatest key's
        /// value, which neither path needs, fails neither; and a value the
        /// general put reads, an ancestor's, fails both.
        #[test]
        fn the_append_path_reads_no_page_the_general_put_does_not() {
            let key = |path: &[&[u8]]| crate::tree::key::encode(path);
            let cases = [
                (
                    "the greatest key's value",
                    vec![(KIND_VALUE, key(&[b"m"]), vec![7; 6000])],
                    vec![
                        put(&[b"n"], Node::Value(vec![1])),
                        put(&[b"o"], Node::Value(vec![2])),
                    ],
                    true,
                ),
                (
                    "an ancestor's value",
                    vec![
                        (crate::page::KIND_BOTH, key(&[b"a"]), vec![7; 6000]),
                        (KIND_VALUE, key(&[b"a", b"x"]), vec![1]),
                    ],
                    vec![
                        put(&[b"a", b"y"], Node::Value(vec![1])),
                        put(&[b"a", b"z"], Node::Value(vec![2])),
                    ],
                    false,
                ),
            ];
            for (name, items, edits, succeeds) in cases {
                let mut outcomes = Vec::new();
                for appends in [false, true] {
                    let store = MemStore::new(Numbering::Sequential);
                    let built = crate::tree::built::tree(&store, std::slice::from_ref(&items), &[]);
                    assert!(!built.overflow[0].is_empty(), "{name}: the value spills");
                    for &page in &built.overflow[0] {
                        store.fail_reads(page, u32::MAX);
                    }
                    let mut unit = store.begin().unwrap();
                    outcomes.push(
                        apply_with(&store, &mut unit, Some(built.root), 2, &edits, appends)
                            .map(|root| root.hash),
                    );
                }
                assert_eq!(outcomes[0].is_ok(), succeeds, "{name}: {:?}", outcomes[0]);
                assert_eq!(outcomes[0], outcomes[1], "{name}");
            }
        }

        /// Review of #769, rule 1: on trees the writer never makes, read
        /// from their pages, the append path gives what the general put
        /// gives. Each case's first put takes the general put, which looks
        /// at the tree; later ones append once the greatest key is proven.
        #[test]
        fn trees_the_writer_never_makes_give_what_the_general_put_gives() {
            let key = |path: &[&[u8]]| crate::tree::key::encode(path);
            let item = |path: &[&[u8]], kind: u8, v: &[u8]| (kind, key(path), v.to_vec());
            let long = |tail: &[u8]| [&[b'p'; 1200][..], tail].concat();
            // A name, the leaves and their separators, the edits, and the
            // puts the append path makes.
            type Case = (&'static str, Leaves, Vec<Vec<u8>>, Vec<Edit>, usize);
            type Leaves = Vec<Vec<crate::tree::built::Item>>;
            let cases: Vec<Case> = vec![
                (
                    "a key without its parent",
                    vec![vec![item(&[b"a", b"x"], KIND_VALUE, b"v")]],
                    vec![],
                    vec![
                        put(&[b"a", b"y"], Node::Value(vec![1])),
                        put(&[b"a", b"z"], Node::Value(vec![2])),
                    ],
                    1,
                ),
                (
                    "a value with a child",
                    vec![vec![
                        item(&[b"a"], KIND_VALUE, b"v"),
                        item(&[b"a", b"x"], KIND_VALUE, b"w"),
                    ]],
                    vec![],
                    vec![
                        put(&[b"a", b"y"], Node::Value(vec![1])),
                        put(&[b"a", b"z"], Node::Value(vec![2])),
                    ],
                    1,
                ),
                (
                    "a key outside its leaf's range",
                    vec![
                        vec![item(&[b"z"], KIND_VALUE, b"v")],
                        vec![
                            item(&[b"c"], KIND_VALUE, b"v"),
                            item(&[b"d"], KIND_VALUE, b"v"),
                        ],
                    ],
                    vec![b"b".to_vec()],
                    vec![
                        put(&[b"e"], Node::Value(vec![1])),
                        put(&[b"f"], Node::Value(vec![2])),
                        put(&[b"g"], Node::Value(vec![3])),
                    ],
                    2,
                ),
                (
                    "spilled keys out of order",
                    vec![vec![
                        item(&[&long(b"z")], KIND_VALUE, &[7; 3000]),
                        item(&[&long(b"c")], KIND_VALUE, &[7; 3000]),
                    ]],
                    vec![],
                    vec![
                        put(&[&long(b"m")], Node::Value(vec![1])),
                        put(&[&long(b"n")], Node::Value(vec![2])),
                    ],
                    0,
                ),
                // Review of #769, round 2, finding 2: the general put finds
                // `long(n)` where the leaf's disorder puts it and sets
                // nothing; an append past `long(m)` would set it anew.
                (
                    "spilled keys out of order, one put again",
                    vec![[b"c", b"d", b"e", b"n", b"f"]
                        .iter()
                        .map(|tail| item(&[&long(*tail)], KIND_VALUE, &[7; 3000]))
                        .collect()],
                    vec![],
                    vec![
                        put(&[&long(b"m")], Node::Value(vec![1])),
                        put(&[&long(b"n")], Node::Value(vec![7; 3000])),
                    ],
                    0,
                ),
                // Round 2, finding 1: a right leaf holding keys below its
                // separator. Appended there, `b/l` and `b/m` would split it
                // between `a3` and `b/k`, under a separator `b` below `b/a`,
                // and the general put's next lookup of `b` would go to the
                // new leaf and set `b` again there.
                (
                    "a right leaf below its separator, split",
                    vec![
                        vec![item(&[b"b"], crate::page::KIND_LIST, b"")],
                        [b"a1", b"a2", b"a3"]
                            .iter()
                            .map(|k| item(&[*k], KIND_VALUE, &[7; 3490]))
                            .collect(),
                    ],
                    vec![key(&[b"b", b"a"])],
                    vec![
                        put(&[b"b", b"k"], Node::Value(vec![1])),
                        put(&[b"b", b"l"], Node::Value(vec![2; 3490])),
                        put(&[b"b", b"m"], Node::Value(vec![3; 3490])),
                        put(&[b"b", b"n"], Node::Value(vec![4])),
                        Edit::delete(vec![b"b".to_vec(), b"l".to_vec()]),
                        Edit::delete(vec![b"b".to_vec(), b"m".to_vec()]),
                    ],
                    0,
                ),
                // A key without its parent under appends that split the
                // right leaf again and again and the root index above it:
                // keys of 3,900 bytes make separators that long, four to an
                // index page.
                (
                    "a key without its parent, split up to the root",
                    vec![
                        vec![item(&[b"a"], KIND_VALUE, b"v")],
                        vec![item(&[b"m", b"a"], KIND_VALUE, b"v")],
                    ],
                    vec![key(&[b"m"])],
                    (0..40u32)
                        .map(|i| {
                            let child =
                                [&[b't'; 3900][..], &(i + 0x0101_0101).to_be_bytes()].concat();
                            put(&[b"m", &child], Node::Value(vec![1]))
                        })
                        .collect(),
                    39,
                ),
            ];
            for (name, leaves, seps, edits, appends) in cases {
                let seps: Vec<&[u8]> = seps.iter().map(Vec::as_slice).collect();
                let general = MemStore::new(Numbering::Sequential);
                let root = crate::tree::built::tree(&general, &leaves, &seps).root;
                let expected = from_memory(&general, Some(root), &edits, &[], false);
                let store = MemStore::new(Numbering::Sequential);
                let root = crate::tree::built::tree(&store, &leaves, &seps).root;
                let (got, made) = appended(|| from_memory(&store, Some(root), &edits, &[], true));
                let hashes = |roots: &[Root]| roots.iter().map(|r| r.hash).collect::<Vec<_>>();
                assert_eq!(hashes(&got), hashes(&expected), "{name}");
                assert_eq!(made, appends, "{name}: puts the append path made");
                // The roots #759's `apply` gives for round 2's inputs, run
                // at deaee759a.
                let at_759 = match name {
                    "spilled keys out of order, one put again" => Some("Hash(9412f12652d6ccf1)"),
                    "a right leaf below its separator, split" => Some("Hash(449b12a0dfa00030)"),
                    _ => None,
                };
                if let (Some(at_759), Some(last)) = (at_759, expected.last()) {
                    assert_eq!(format!("{:?}", last.hash), at_759, "{name}");
                }
                if name == "a key without its parent, split up to the root" {
                    let top = store.read(got[0].page, &got[0].hash).unwrap();
                    assert_eq!(top.level(), 2, "{name}: the root index split");
                }
            }
        }

        /// Review of #769, round 2: an append that splits a page drops the
        /// proof, so the next put proves the greatest key again (and looks
        /// its ancestors up again) before it appends.
        #[test]
        fn an_append_that_splits_a_page_proves_the_greatest_key_again() {
            let edits: Vec<Edit> = (0..200u32)
                .map(|i| put(&[b"r", &i.to_be_bytes()], Node::Value(vec![1; 1000])))
                .collect();
            let ((got, made), proofs) = proved(|| appended(|| on_memory(&edits, &[], true)));
            assert_eq!(got, on_memory(&edits, &[], false));
            // Some 200 KB of items fill a dozen leaves or more: a split every
            // fifteen puts or so, and a proof after each.
            assert!(made > 190, "{made} appends");
            assert!(proofs >= 12, "{proofs} proofs");
        }
    }

    #[test]
    fn separators_are_the_shortest_that_separate() {
        assert_eq!(shortest_separator(b"abc", b"abd"), b"abd");
        assert_eq!(shortest_separator(b"abc", b"b"), b"b");
        // The left key a proper prefix of the right: the right key cut just
        // after the byte that follows the left key.
        assert_eq!(shortest_separator(b"ab", b"abc"), b"abc");
        assert_eq!(
            shortest_separator(b"z\x00\x01", b"z\x00\x01a\x00\x01"),
            b"z\x00\x01a"
        );
        assert_eq!(
            shortest_separator(b"a\x00\x01", b"a\x00\xff\x00\x01"),
            b"a\x00\xff"
        );
    }

    #[test]
    fn a_split_prefers_the_middle_and_an_append_fills_its_page() {
        let sizes = [4000, 4000, 4000, 4000, 2000];
        let total: usize = sizes.iter().sum();
        let prefix = |s: usize| sizes[..s].iter().sum::<usize>();
        let s = choose(sizes.len(), None, prefix, |s| total - prefix(s)).unwrap();
        assert_eq!(s, 2);
        let full = [4000, 4000, 4000, 3000, 100];
        let total: usize = full.iter().sum();
        let prefix = |s: usize| full[..s].iter().sum::<usize>();
        let s = choose(full.len(), Some(4), prefix, |s| total - prefix(s)).unwrap();
        assert_eq!(s, 4);
    }
}
