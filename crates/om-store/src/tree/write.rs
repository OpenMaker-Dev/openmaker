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

use std::sync::Arc;

use super::key;
use super::read::{Pages, Result};
use super::{check_edits, fault, Edit, Error, Node, Root};
use crate::page::{
    self, Builder, Fault, Hash, Page, PageKind, PageNo, PageStore, Release, CHILD_FOOTPRINT,
    KIND_BOTH, KIND_LIST, KIND_VALUE, MAX_LEVEL, OVERFLOW_DATA, SPILL_INLINE, USABLE,
};

/// A node of a page whose footprint shrank below this is joined with a
/// sibling.
const MIN_FILL: usize = USABLE / 4;
/// A node over capacity is split before the unit a primitive changed when
/// what comes before that unit fills at least this much of a page.
const SPLIT_HIGH: usize = USABLE * 7 / 8;

/// A tree with no nodes: one empty leaf, written by entry `entry`.
pub fn create<S: PageStore + ?Sized>(store: &S, entry: u64) -> Result<Root> {
    let r = write_page(store, Builder::new(PageKind::Leaf, 0, entry))?;
    Ok(Root {
        page: r.page,
        hash: r.hash,
    })
}

/// Applies the edits of entry `entry`, in order, to the version `root`
/// names, and returns the root of the new version. The old version's pages
/// are left as they were. An entry whose edits change nothing returns
/// `root` and writes nothing.
///
/// Every edit is checked against the store's bounds before anything is
/// done ([`check_edits`]). `entry` must be above the index of the last entry
/// that changed the tree.
pub fn apply<S: PageStore + ?Sized>(
    store: &S,
    root: Root,
    entry: u64,
    edits: &[Edit],
) -> Result<Root> {
    check_edits(edits)?;
    let pages = Pages { store };
    let top = pages.root(root)?;
    if entry <= top.written() {
        return Err(Error::EntryOrder {
            entry,
            last: top.written(),
        });
    }
    let mut cx = Cx {
        pages,
        entry,
        released: Vec::new(),
        changed: false,
    };
    let mut tree = Work {
        slot: Slot::Clean(ChildRef {
            page: root.page,
            hash: root.hash,
            stamp: top.written(),
        }),
        level: top.level(),
    };
    drop(top);
    for edit in edits {
        match edit {
            Edit::Put { path, node } => put(&mut cx, &mut tree, path, node)?,
            Edit::Delete { path } => {
                let lo = key::encode(path);
                let hi = key::subtree_end(&lo);
                delete(&mut cx, &mut tree, &lo, hi.as_deref())?;
            }
        }
    }
    if !cx.changed {
        return Ok(root);
    }
    // Every entry that changes the tree rewrites its root.
    materialize(&mut cx, &mut tree.slot, tree.level)?;
    let new = write_slot(&mut cx, tree.slot, tree.level)?;
    for (number, hash, how) in &cx.released {
        store.release(*number, hash, *how)?;
    }
    Ok(Root {
        page: new.page,
        hash: new.hash,
    })
}

/// What the entry being applied carries along.
struct Cx<'s, S: ?Sized> {
    pages: Pages<'s, S>,
    entry: u64,
    /// Pages the new version no longer reaches, in the order found: a page
    /// before what it refers to.
    released: Vec<(PageNo, Hash, Release)>,
    changed: bool,
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
/// would cost as much as the rest of a primitive.
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
    /// The key, unless it is all in the page the value is kept from: empty
    /// then (no key is empty), so that copying a leaf does not copy its keys.
    key: Vec<u8>,
    kind: u8,
    value: Value,
}

enum Value {
    /// Set by this entry.
    New(Vec<u8>),
    /// As item `.1` of page `.0` holds it, overflow pages included.
    Kept(Arc<Page>, usize),
}

struct WSep {
    key: Vec<u8>,
    /// The page and child it was read from, to keep its overflow pages.
    kept: Option<(Arc<Page>, usize)>,
}

impl WItem {
    fn key(&self) -> &[u8] {
        match &self.value {
            Value::Kept(page, i) if self.key.is_empty() => page.item(*i).key_part().0,
            _ => &self.key,
        }
    }

    fn value_len(&self) -> usize {
        match &self.value {
            Value::New(v) => v.len(),
            Value::Kept(page, i) => page.item(*i).value_len,
        }
    }

    fn footprint(&self) -> usize {
        page::item_footprint(self.key().len(), self.value_len())
    }
}

impl WSep {
    fn footprint(&self) -> usize {
        page::separator_footprint(self.key.len())
    }
}

impl WLeaf {
    fn new(items: Vec<WItem>) -> WLeaf {
        let size = items.iter().map(WItem::footprint).sum();
        WLeaf { items, size }
    }

    fn footprint(&self) -> usize {
        debug_assert_eq!(
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
        debug_assert_eq!(
            self.size,
            self.children.len() * CHILD_FOOTPRINT
                + self.seps.iter().map(WSep::footprint).sum::<usize>()
        );
        self.size
    }

    /// The child whose range holds `key`.
    fn child_for(&self, key: &[u8]) -> usize {
        self.seps.partition_point(|s| s.key.as_slice() <= key)
    }

    /// The last child whose range starts below `key`.
    fn last_below(&self, key: &[u8]) -> usize {
        self.seps.partition_point(|s| s.key.as_slice() < key)
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

/// The working node in `slot`, reading and copying its page first if the
/// entry has not touched it yet.
fn materialize<'a, S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    slot: &'a mut Slot,
    level: u8,
) -> Result<&'a mut WNode> {
    if let Slot::Clean(r) = slot {
        let r = *r;
        let page = cx.pages.at(r.page, r.hash, r.stamp, level)?;
        let node = decode(cx, &page)?;
        cx.released.push((r.page, r.hash, Release::Copied));
        *slot = Slot::Dirty(Box::new(node));
    }
    match slot {
        Slot::Dirty(node) => Ok(node),
        Slot::Clean(_) => Err(internal("a page that could not be copied")),
    }
}

fn decode<S: PageStore + ?Sized>(cx: &Cx<'_, S>, page: &Arc<Page>) -> Result<WNode> {
    match page.kind() {
        PageKind::Leaf => {
            let mut items = Vec::with_capacity(page.count());
            for i in 0..page.count() {
                let item = page.item(i);
                let key = if item.key_part().1 {
                    Vec::new()
                } else {
                    cx.pages.item_key(page, i)?
                };
                items.push(WItem {
                    key,
                    kind: item.kind,
                    value: Value::Kept(page.clone(), i),
                });
            }
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
                    seps.push(WSep {
                        key: cx.pages.separator(page, i)?,
                        kept: Some((page.clone(), i)),
                    });
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
fn drop_value<S: ?Sized>(cx: &mut Cx<'_, S>, value: &Value) {
    if let Value::Kept(page, i) = value {
        for (number, hash) in page.item(*i).refs.iter() {
            cx.released.push((number, hash, Release::Dropped));
        }
    }
}

fn drop_separator<S: ?Sized>(cx: &mut Cx<'_, S>, sep: &WSep) {
    if let Some((page, i)) = &sep.kept {
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
                    drop_value(cx, &item.value);
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
    key: &[u8],
) -> Result<Option<Node>> {
    match slot {
        Slot::Clean(r) => cx
            .pages
            .lookup(cx.pages.at(r.page, r.hash, r.stamp, level)?, key),
        Slot::Dirty(node) => match &**node {
            WNode::Leaf(leaf) => match leaf.items.binary_search_by(|it| it.key().cmp(key)) {
                Ok(i) => Ok(Some(node_of(cx, &leaf.items[i])?)),
                Err(_) => Ok(None),
            },
            WNode::Index(ix) => match ix.children.get(ix.child_for(key)) {
                Some(child) => existing(cx, child, level - 1, key),
                None => Ok(None),
            },
        },
    }
}

fn node_of<S: PageStore + ?Sized>(cx: &Cx<'_, S>, item: &WItem) -> Result<Node> {
    let value = match &item.value {
        Value::New(v) => v.clone(),
        Value::Kept(page, i) => cx.pages.item_node(page, *i)?.value().to_vec(),
    };
    Ok(Node::from_kind(item.kind, value))
}

fn first_from<S: PageStore + ?Sized>(
    cx: &Cx<'_, S>,
    slot: &Slot,
    level: u8,
    lo: &[u8],
) -> Result<Option<Vec<u8>>> {
    match slot {
        Slot::Clean(r) => cx
            .pages
            .first_from(&*cx.pages.at(r.page, r.hash, r.stamp, level)?, lo),
        Slot::Dirty(node) => match &**node {
            WNode::Leaf(leaf) => {
                let i = leaf.items.partition_point(|it| it.key() < lo);
                Ok(leaf.items.get(i).map(|it| it.key().to_vec()))
            }
            WNode::Index(ix) => {
                for child in &ix.children[ix.child_for(lo)..] {
                    if let Some(key) = first_from(cx, child, level - 1, lo)? {
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
    hi: Option<&[u8]>,
) -> Result<Option<Vec<u8>>> {
    match slot {
        Slot::Clean(r) => cx
            .pages
            .last_before(&*cx.pages.at(r.page, r.hash, r.stamp, level)?, hi),
        Slot::Dirty(node) => match &**node {
            WNode::Leaf(leaf) => {
                let items = &leaf.items;
                let n = hi.map_or(items.len(), |hi| items.partition_point(|it| it.key() < hi));
                Ok(n.checked_sub(1).map(|i| items[i].key().to_vec()))
            }
            WNode::Index(ix) => {
                if ix.children.is_empty() {
                    return Ok(None);
                }
                let last = hi.map_or(ix.children.len() - 1, |hi| ix.last_below(hi));
                for child in ix.children[..=last].iter().rev() {
                    if let Some(key) = last_before(cx, child, level - 1, hi)? {
                        return Ok(Some(key));
                    }
                }
                Ok(None)
            }
        },
    }
}

fn has_keys<S: PageStore + ?Sized>(
    cx: &Cx<'_, S>,
    slot: &Slot,
    level: u8,
    lo: &[u8],
    hi: Option<&[u8]>,
) -> Result<bool> {
    Ok(match first_from(cx, slot, level, lo)? {
        Some(key) => hi.is_none_or(|hi| key.as_slice() < hi),
        None => false,
    })
}

// The edits.

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
    // both (as `om_hbam::OwnedNode::ensure` does).
    let mut key = Vec::new();
    for segment in ancestors {
        key::push_segment(&mut key, segment);
        match existing(cx, &tree.slot, tree.level, &key)? {
            None => set(
                cx,
                tree,
                Op::Set(WItem {
                    key: key.clone(),
                    kind: KIND_LIST,
                    value: Value::New(Vec::new()),
                }),
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
    if existing(cx, &tree.slot, tree.level, &key)?.as_ref() != Some(node) {
        let value = node.value().to_vec();
        set(
            cx,
            tree,
            Op::Set(WItem {
                key,
                kind: node.kind(),
                value: Value::New(value),
            }),
        )?;
    }
    Ok(())
}

/// A change to one item.
enum Op {
    /// Insert or replace an item.
    Set(WItem),
    /// The item with this key holds a value; make it hold both.
    Both(Vec<u8>),
}

impl Op {
    fn key(&self) -> &[u8] {
        match self {
            Op::Set(item) => item.key(),
            Op::Both(key) => key,
        }
    }
}

/// What a primitive did to a node, for its parent: the position of the
/// unit it changed, and whether the node's footprint shrank.
#[derive(Clone, Copy)]
struct Change {
    hint: usize,
    shrank: bool,
}

fn set<S: PageStore + ?Sized>(cx: &mut Cx<'_, S>, tree: &mut Work, op: Op) -> Result<()> {
    cx.changed = true;
    let change = set_in(cx, &mut tree.slot, tree.level, op)?;
    fix_root(tree, Some(change.hint))
}

fn set_in<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    slot: &mut Slot,
    level: u8,
    op: Op,
) -> Result<Change> {
    let node = materialize(cx, slot, level)?;
    match node {
        WNode::Leaf(leaf) => {
            let found = leaf.items.binary_search_by(|it| it.key().cmp(op.key()));
            let (hint, shrank) = match (op, found) {
                (Op::Set(item), Ok(i)) => {
                    let size = item.footprint();
                    let old = std::mem::replace(&mut leaf.items[i], item);
                    let was = old.footprint();
                    leaf.size = leaf.size - was + size;
                    drop_value(cx, &old.value);
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
            Ok(Change { hint, shrank })
        }
        WNode::Index(ix) => {
            let before = ix.footprint();
            let c = ix.child_for(op.key());
            if c >= ix.children.len() {
                return Err(internal("an index node without children"));
            }
            let change = set_in(cx, &mut ix.children[c], level - 1, op)?;
            let fixed = fix_child(cx, ix, c, change)?;
            Ok(Change {
                hint: fixed.hint(),
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
    let Some(first) = first_from(cx, &tree.slot, tree.level, lo)? else {
        return Ok(false);
    };
    if hi.is_some_and(|hi| first.as_slice() >= hi) {
        return Ok(false);
    }
    let last = last_before(cx, &tree.slot, tree.level, hi)?.unwrap_or_else(|| first.clone());
    cx.changed = true;
    delete_in(cx, &mut tree.slot, tree.level, lo, hi)?;
    fix_root(tree, None)?;
    // The deleted keys' places now belong to the leaves around them, which
    // may be pages this primitive did not otherwise touch: they are
    // rewritten so that their stamps cover the deletion (format section
    // 2.8).
    touch(cx, &mut tree.slot, tree.level, &first)?;
    touch(cx, &mut tree.slot, tree.level, &last)?;
    Ok(true)
}

/// Deletes `[lo, hi)` from the subtree in `slot`, which holds keys in it.
fn delete_in<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    slot: &mut Slot,
    level: u8,
    lo: &[u8],
    hi: Option<&[u8]>,
) -> Result<Change> {
    let node = materialize(cx, slot, level)?;
    match node {
        WNode::Leaf(leaf) => {
            let items = &mut leaf.items;
            let start = items.partition_point(|it| it.key() < lo);
            let end = hi.map_or(items.len(), |hi| items.partition_point(|it| it.key() < hi));
            for item in items.drain(start..end.max(start)) {
                leaf.size -= item.footprint();
                drop_value(cx, &item.value);
            }
            Ok(Change {
                hint: start,
                shrank: end > start,
            })
        }
        WNode::Index(ix) => {
            if ix.children.is_empty() {
                return Err(internal("an index node without children"));
            }
            let before = ix.footprint();
            let i = ix.child_for(lo);
            let j = hi.map_or(ix.children.len() - 1, |hi| ix.last_below(hi));
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
                changed.push((i, delete_in(cx, &mut ix.children[i], level - 1, lo, hi)?));
            }
            if let Some(r) = right {
                if has_keys(cx, &ix.children[r], level - 1, lo, hi)? {
                    changed.push((r, delete_in(cx, &mut ix.children[r], level - 1, lo, hi)?));
                }
            }
            // Fix the changed children left to right. Fixing the left one
            // can take in the right one.
            let mut hint = i;
            let mut done: Option<Fixed> = None;
            for (c, change) in changed {
                let c = match &done {
                    Some(f) if c >= f.start + f.old => c - f.old + f.new,
                    Some(f) if c >= f.start => continue,
                    _ => c,
                };
                let fixed = fix_child(cx, ix, c, change)?;
                hint = fixed.hint();
                done = Some(fixed);
            }
            Ok(Change {
                hint,
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
    key: &[u8],
) -> Result<()> {
    match materialize(cx, slot, level)? {
        WNode::Leaf(_) => Ok(()),
        WNode::Index(ix) => {
            if ix.children.is_empty() {
                return Ok(());
            }
            let c = ix.child_for(key);
            touch(cx, &mut ix.children[c], level - 1, key)
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
    /// Where the parent's change is: its first new child, if it gained any.
    fn hint(&self) -> usize {
        if self.new > self.old {
            self.start + 1
        } else {
            self.start
        }
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
        let parts = split(node, Some(change.hint))?;
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
    materialize(cx, &mut ix.children[l], level)?;
    materialize(cx, &mut ix.children[r], level)?;
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
        let parts = split(node, None)?;
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
fn split(node: WNode, hint: Option<usize>) -> Result<Vec<(Option<WSep>, WNode)>> {
    let mut parts = Vec::new();
    split_into(node, hint, None, &mut parts)?;
    Ok(parts)
}

fn split_into(
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
            let up = WSep {
                key: shortest_separator(items[s - 1].key(), right[0].key()),
                kept: None,
            };
            split_into(WNode::Leaf(WLeaf::new(items)), None, sep, parts)?;
            split_into(WNode::Leaf(WLeaf::new(right)), None, Some(up), parts)
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
            split_into(WNode::Index(ix), None, sep, parts)?;
            split_into(
                WNode::Index(WIndex::new(level, right_children, right_seps)),
                None,
                Some(up),
                parts,
            )
        }
    }
}

/// Where to split `n` units, given the footprints of the two sides of a
/// split before unit `s`. Before the changed unit `hint` when what precedes
/// it fills at least [`SPLIT_HIGH`] and both sides fit (an append fills its
/// page); otherwise where the two sides are nearest equal, the first such
/// place on a tie.
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
/// `right` cut just after the first byte where they differ.
fn shortest_separator(left: &[u8], right: &[u8]) -> Vec<u8> {
    let common = left.iter().zip(right).take_while(|(a, b)| a == b).count();
    right[..(common + 1).min(right.len())].to_vec()
}

/// After a primitive: a root over capacity is split under a new root; an
/// index root with one child is replaced by that child, and one with none
/// by an empty leaf.
fn fix_root(tree: &mut Work, hint: Option<usize>) -> Result<()> {
    loop {
        let Slot::Dirty(node) = &mut tree.slot else {
            return Ok(());
        };
        if node.footprint() > USABLE {
            if tree.level >= MAX_LEVEL {
                return Err(Error::TooTall);
            }
            let node = std::mem::replace(&mut **node, WNode::Leaf(WLeaf::new(Vec::new())));
            let parts = split(node, hint)?;
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

fn write_page<S: PageStore + ?Sized>(store: &S, builder: Builder) -> Result<ChildRef> {
    let number = store.allocate()?;
    let page = builder
        .finish(number)
        .map_err(|f| fault(number, Hash::default(), f))?;
    let r = ChildRef {
        page: number,
        hash: page.hash(),
        stamp: page.written(),
    };
    store.write(Arc::new(page))?;
    Ok(r)
}

/// Writes `stream[SPILL_INLINE..]` to new overflow pages.
fn write_overflow<S: PageStore + ?Sized>(
    cx: &Cx<'_, S>,
    stream: &[u8],
) -> Result<Vec<(PageNo, Hash)>> {
    let mut refs = Vec::new();
    for chunk in stream[SPILL_INLINE..].chunks(OVERFLOW_DATA) {
        let mut builder = Builder::new(PageKind::Overflow, 0, cx.entry);
        builder.data(chunk);
        let r = write_page(cx.pages.store, builder)?;
        refs.push((r.page, r.hash));
    }
    Ok(refs)
}

fn write_slot<S: PageStore + ?Sized>(
    cx: &mut Cx<'_, S>,
    slot: Slot,
    level: u8,
) -> Result<ChildRef> {
    let node = match slot {
        Slot::Clean(r) => return Ok(r),
        Slot::Dirty(node) => *node,
    };
    match node {
        WNode::Leaf(leaf) => {
            let mut builder = Builder::new(PageKind::Leaf, 0, cx.entry);
            for item in &leaf.items {
                match &item.value {
                    Value::New(value) => {
                        let mut stream = Vec::with_capacity(item.key.len() + value.len());
                        stream.extend_from_slice(&item.key);
                        stream.extend_from_slice(value);
                        match page::item_inline(item.key.len(), value.len()) {
                            Some(inline) => {
                                let refs = write_overflow(cx, &stream)?;
                                builder.item(
                                    item.kind,
                                    item.key.len(),
                                    value.len(),
                                    &stream[..inline],
                                    &refs,
                                );
                            }
                            None => {
                                builder.item(item.kind, item.key.len(), value.len(), &stream, &[])
                            }
                        }
                    }
                    Value::Kept(page, i) => {
                        let kept = page.item(*i);
                        let refs: Vec<_> = kept.refs.iter().collect();
                        builder.item(item.kind, kept.key_len, kept.value_len, kept.inline, &refs);
                    }
                }
            }
            write_page(cx.pages.store, builder)
        }
        WNode::Index(ix) => {
            let mut children = Vec::with_capacity(ix.children.len());
            for child in ix.children {
                children.push(write_slot(cx, child, level - 1)?);
            }
            let mut builder = Builder::new(PageKind::Index, level, cx.entry);
            for (t, child) in children.iter().enumerate() {
                if t == 0 {
                    builder.child(child.page, &child.hash, child.stamp, None);
                    continue;
                }
                let sep = &ix.seps[t - 1];
                let kept = sep
                    .kept
                    .as_ref()
                    .and_then(|(page, i)| page.child(*i).separator)
                    .filter(|s| s.refs.len() > 0);
                match (kept, page::separator_inline(sep.key.len())) {
                    (Some(kept), _) => {
                        let refs: Vec<_> = kept.refs.iter().collect();
                        builder.child(
                            child.page,
                            &child.hash,
                            child.stamp,
                            Some((kept.len, kept.inline, &refs)),
                        );
                    }
                    (None, Some(inline)) => {
                        let refs = write_overflow(cx, &sep.key)?;
                        builder.child(
                            child.page,
                            &child.hash,
                            child.stamp,
                            Some((sep.key.len(), &sep.key[..inline], &refs)),
                        );
                    }
                    (None, None) => {
                        builder.child(
                            child.page,
                            &child.hash,
                            child.stamp,
                            Some((sep.key.len(), &sep.key, &[])),
                        );
                    }
                }
            }
            write_page(cx.pages.store, builder)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_are_the_shortest_that_separate() {
        assert_eq!(shortest_separator(b"abc", b"abd"), b"abd");
        assert_eq!(shortest_separator(b"abc", b"b"), b"b");
        assert_eq!(shortest_separator(b"ab", b"abc"), b"abc");
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
