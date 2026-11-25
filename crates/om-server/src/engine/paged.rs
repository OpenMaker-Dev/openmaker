//! The store engine's file (docs/server-storage.md 5, "The store engine's
//! view"; 6.10): an `.fmp12`'s key tree held in a tree of `om_store`'s
//! pages, read as a [`View`], filled from an `.fmp12` ([`import`]) and
//! written back as one ([`export`]).
//!
//! The module is named for what sets this engine apart from the file
//! engine: its file is held in pages, read a page at a time, where the file
//! engine holds the whole tree in memory (`crate::store::Store`, the file
//! engine's store, which this module does not touch).
//!
//! **The view.** [`PagedView`] reads one version of a tree, named by its
//! root, through any `om_store` page store ([`PageStore`]): the page file
//! through a pin's roots, or [`om_store::page::MemStore`]. Every page it
//! reads is checked by the hash its parent holds (`om_store::tree`), and a
//! page that does not read or check is a [`ReadError::Unreadable`] naming
//! the path read, never data. It gives owned values and holds a page only
//! while a call reads it: each call opens the version again from its root,
//! which the page store's cache answers. It answers every read as the file
//! engine's view of the same file does, page for page: the same children,
//! subtrees and walk steps in each call, the same "more may follow"
//! (a listing looks one child past a full page to say), the same limits
//! counted the same way. Its [`Version`] is the index of the last entry
//! that changed the tree (the root page's stamp), so a later version of
//! the same tree has a larger one; [`View::changed_since`] is the tree's
//! own (docs/server-storage-format.md 2.8): from the stamps index pages
//! hold, never false when something under the path changed.
//!
//! **A read that fails partway.** A call that has already given some
//! children, subtrees or walk steps when a read fails ends there and gives
//! them, with "more may follow" (`Ok(true)`, or the walk's place to resume
//! after the last step given), as the trait lets a view give fewer than it
//! was asked for. A failed look-ahead past a full page of children answers
//! "more" too, never "no more". So nothing is lost and nothing is made up:
//! the next call starts after what was given and reaches the failed read
//! first, which fails it with [`ReadError::Unreadable`], or reads what is
//! there if the read now succeeds. A call whose first read fails fails.
//! The readers of views read a short page with "more" as such: the
//! iterators ask for the next page, and `security`'s one-child probe asks
//! for one child, which this view gives or fails on.
//!
//! **Import.** [`import`] reads an `.fmp12` with `om_hbam`'s streaming
//! reader ([`NodeStream`]) and puts every node, with its kind and value,
//! empty lists included, into a new tree through `om_store::tree::apply`,
//! in batches of a bounded number of nodes and bytes ([`Batch`]), each one
//! unit of work of the page store, committed on its own ([`Units`]), so
//! that what it holds does not grow with the file. It is a function of a
//! source of bytes and where its units go, and goes through no log and no
//! entry of the store: when the store's write path takes it, its batches
//! become the parts of a group whose last entry adds the file
//! (docs/server-storage.md 6.9, 6.10), and the entry indexes it now takes
//! from its caller come from the log. What it accepts, [`export`] gives
//! back ([`import`] says exactly what that promises): its export succeeds,
//! and holds the same nodes, kinds and values. It refuses, with
//! a typed error ([`ImportError`]), and with the tree it made dropped
//! again:
//!
//! - a file that does not read as an `.fmp12`, the encrypted ones among
//!   them: OpenMaker does not read encrypted files, and its readers find
//!   one as a page that does not decode (docs/re/file-format.md 6), as the
//!   file engine refuses to host it. The streaming reader also refuses five
//!   shapes the file engine's reader (`om_hbam::HbamFile`) reads and the
//!   file engine hosts (`om_hbam::stream`'s module doc): a leaf chain that
//!   ends before its stream closes the root list (a file cut short), one
//!   that goes on after it closed the root list, a page that does not push
//!   again the path left open before it, a value given to a key after its
//!   children, and a page larger than 64 KiB;
//! - a file holding content `om_hbam`'s writer cannot write
//!   (`om_hbam::writer::unwritable`: a key segment or a path too long, a
//!   list holding a value, an item no page can hold), or writes in another
//!   shape (a list that carries an empty value, below, and a value over
//!   1,000 bytes held whole, which it writes in pieces), or whose page 0 it
//!   does not write (not one 4,096-byte page, or a change number with no
//!   room left for the pages an export numbers): the file engine hosts
//!   such a file, read-only where it cannot write it, and exports its own
//!   bytes (section 2, rule 2); the store keeps that image only once its
//!   kept original images are built (section 6.10);
//! - a node whose entry in an export's index could cost more than
//!   `om_hbam::writer::MAX_INDEX_ENTRY`, 1,019 bytes (a path of long
//!   segments, deep): within it the writer's index never passes the 16
//!   levels it writes, and past it an index with too little fan-out for
//!   its pages does, and the export fails. The bound is conservative: some
//!   files past it could be written. The file engine hosts such a file and
//!   its saves fail only when they meet the 16 levels;
//! - an item past the store's bounds (section 6.11), which no file the
//!   reader reads can hold: a slot's value has a 2-byte length and a path
//!   at most 17 segments of at most 255 bytes, within the 64 KiB of each;
//! - a file whose entries could not all be numbered after the first one it
//!   is given, below the last index an entry can have: a bound worked out
//!   from the file's length before anything is written.
//!
//! **Page 0.** [`Imported::page_zero`] is the file's page 0 as it was
//! read. The file engine keeps it in its derived state
//! (`Derived::page_zero`); the store keeps it in the file's row of its own
//! tree (`[03, id, 03]`, docs/server-storage-format.md 5.3), which the
//! entry that adds the file writes. Until the import goes through entries,
//! its caller holds it and gives it to [`export`].
//!
//! **A list that carries an empty value**
//! ([#754](https://github.com/OpenMaker-Dev/openmaker/issues/754)): the
//! reader gives such a key as a list with an empty value and the file
//! engine keeps it so in memory, but its writer writes a bare list, so an
//! export could not give it back. Until #754 settles which of the two a
//! file holds, the import refuses it, naming its path. No file FileMaker
//! wrote holds one (`om-hbam` `tests/templates.rs`,
//! `no_shipped_file_holds_a_list_with_a_value`).
//!
//! **Export.** [`export`] is the file engine's written export
//! (`super::export`): a walk of the view into `om_hbam`'s streaming writer,
//! read back and compared with a second walk, with the imported page 0 and
//! its own change number, so that it is the file engine's compacted copy
//! of the same file byte for byte. A read of the view that fails is
//! [`ExportError::Read`], and what a failed export leaves under its name
//! is what [`ExportError`] says for every export
//! ([#761](https://github.com/OpenMaker-Dev/openmaker/issues/761)).

use std::borrow::Cow;
use std::fmt;
use std::io::{Read as IoRead, Seek};
use std::ops::Bound;

use om_hbam::stream::NodeStream;
use om_hbam::writer::{index_entry_cost, MAX_INDEX_ENTRY, MAX_INLINE_VALUE};
use om_hbam::OwnedNode;
use om_store::page::{EntryPages, MemStore, PageStore, Root, TreeName};
use om_store::tree::{self, key, Edit, Node, Tree};

use super::export::{self, ExportError, Exported, Progress, Target};
use super::view::{owned_path, Child, Direction, Head, ReadError, Step, Version, View, NO_LIMIT};

/// One version of a file's tree in a page store (the module doc). A call
/// whose read fails after it has given something gives that with "more may
/// follow", never "no more", and the next call fails on the same read (the
/// module doc, "A read that fails partway").
pub struct PagedView<'s, S: ?Sized> {
    store: &'s S,
    root: Root,
    version: Version,
}

impl<'s, S: PageStore + ?Sized> PagedView<'s, S> {
    /// The version of a tree whose root is `root` in `store`: its root page
    /// is read and checked here, for its stamp, the view's version.
    pub fn open(store: &'s S, root: Root) -> Result<Self, ReadError> {
        let tree = Tree::open(store, root).map_err(|e| unreadable(&[], &e))?;
        Ok(PagedView {
            store,
            root,
            version: tree.last_entry(),
        })
    }

    /// The version, opened again from its root for one call.
    fn tree(&self, path: &[&[u8]]) -> Result<Tree<'s, S>, ReadError> {
        Tree::open(self.store, self.root).map_err(|e| unreadable(path, &e))
    }

    /// Whether the root has a child.
    fn root_has_children(&self, tree: &Tree<'s, S>) -> Result<bool, ReadError> {
        let empty: &[&[u8]] = &[];
        match tree.walk(empty).next() {
            None => Ok(false),
            Some(Ok(_)) => Ok(true),
            Some(Err(e)) => Err(unreadable(&[], &e)),
        }
    }

    /// The subtree at `path` (the module doc of `super::view`, "Limits"),
    /// built from a walk of its keys, every list with exactly as many places
    /// as children so that it is counted as the file engine's tree of the
    /// same file is.
    fn build(
        &self,
        tree: &Tree<'s, S>,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<OwnedNode>, ReadError> {
        let base = path.len();
        let too_large = || ReadError::TooLarge {
            path: owned_path(path),
            limit,
        };
        // Below what the tree will take, counted as the nodes come, so that
        // a large subtree is refused before it is read whole.
        let mut least = size_of::<OwnedNode>();
        let mut stack: Vec<OwnedNode> = Vec::new();
        let mut items = tree.walk(path);
        if path.is_empty() {
            // The root is the file's list itself: no key, no value, never
            // written as a push (as `om_hbam::KeyTree` makes it).
            stack.push(OwnedNode::default());
        } else {
            match items.next_lent(|key, node| (key_depth(key), node)) {
                None => return Ok(None),
                Some(Err(e)) => return Err(unreadable(path, &e)),
                Some(Ok((depth, node))) => {
                    if depth != base {
                        return Err(orphan(path));
                    }
                    let segment = exact(path[base - 1].to_vec());
                    least += segment.len() + node.value().len();
                    stack.push(owned(segment, node));
                }
            }
        }
        // Each key is lent from its page: only its last segment is copied.
        while let Some(item) = items.next_lent(|key, node| {
            let (depth, segment) = last_segment(key);
            (depth, segment, node)
        }) {
            let (depth, segment, node) = item.map_err(|e| unreadable(path, &e))?;
            let below = depth.checked_sub(base).filter(|d| *d > 0);
            let Some(below) = below.filter(|d| *d <= stack.len()) else {
                return Err(orphan(path));
            };
            while stack.len() > below {
                finish(&mut stack);
            }
            least += size_of::<OwnedNode>() + segment.len() + node.value().len();
            if limit != NO_LIMIT && least > limit {
                return Err(too_large());
            }
            stack.push(owned(segment, node));
        }
        while stack.len() > 1 {
            finish(&mut stack);
        }
        let mut node = stack.pop().unwrap_or_default();
        node.children = exact(std::mem::take(&mut node.children));
        if limit != NO_LIMIT && om_hbam::tree::memory_within(&node, limit).is_none() {
            return Err(too_large());
        }
        Ok(Some(node))
    }
}

/// The last node on `stack`, made a child of the one below it.
fn finish(stack: &mut Vec<OwnedNode>) {
    if let Some(mut node) = stack.pop() {
        node.children = exact(std::mem::take(&mut node.children));
        if let Some(parent) = stack.last_mut() {
            parent.children.push(node);
        }
    }
}

/// `v` with no room past its length: what an owned tree read from a file
/// holds (`om_hbam::Node::to_owned_node`), which `om_hbam::tree::memory`
/// counts by capacity.
fn exact<T>(v: Vec<T>) -> Vec<T> {
    v.into_boxed_slice().into_vec()
}

/// A tree's node as an owned tree's node, without children.
fn owned(segment: Vec<u8>, node: Node) -> OwnedNode {
    let (push, value) = match node {
        Node::List => (true, None),
        Node::Value(v) => (false, Some(exact(v))),
        Node::Both(v) => (true, Some(exact(v))),
    };
    OwnedNode {
        segment,
        value,
        push,
        children: Vec::new(),
    }
}

/// A tree's node as a view's head.
fn head(node: Node) -> Head<'static> {
    match node {
        Node::List => Head {
            list: true,
            value: None,
        },
        Node::Value(v) => Head {
            list: false,
            value: Some(Cow::Owned(v)),
        },
        Node::Both(v) => Head {
            list: true,
            value: Some(Cow::Owned(v)),
        },
    }
}

/// How many segments the key `key` has, and its last one: its path is not
/// decoded whole for a walk's step.
fn last_segment(key: &[u8]) -> (usize, Vec<u8>) {
    let (mut depth, mut start, mut i) = (0, 0, 0);
    let mut last = (0, 0);
    while i + 1 < key.len() {
        if key[i] == 0 {
            if key[i + 1] == 0x01 {
                depth += 1;
                last = (start, i);
                start = i + 2;
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    let mut segment = Vec::with_capacity(last.1 - last.0);
    let bytes = &key[last.0..last.1];
    let mut j = 0;
    while j < bytes.len() {
        segment.push(bytes[j]);
        // `00 ff` is a zero byte of the segment.
        j += if bytes[j] == 0 { 2 } else { 1 };
    }
    (depth, exact(segment))
}

/// How many segments the key `key` has.
fn key_depth(key: &[u8]) -> usize {
    let mut depth = 0;
    let mut i = 0;
    while i + 1 < key.len() {
        if key[i] == 0 {
            depth += usize::from(key[i + 1] == 0x01);
            i += 2;
        } else {
            i += 1;
        }
    }
    depth
}

/// A read of `path` that reached a page that does not read or check.
fn unreadable(path: &[&[u8]], error: &tree::Error) -> ReadError {
    ReadError::Unreadable {
        path: owned_path(path),
        why: error.to_string(),
    }
}

/// A subtree whose keys do not make a tree: a node without its parent.
fn orphan(path: &[&[u8]]) -> ReadError {
    ReadError::Unreadable {
        path: owned_path(path),
        why: "a node without its parent".into(),
    }
}

fn store_direction(direction: Direction) -> tree::Direction {
    match direction {
        Direction::Forward => tree::Direction::Forward,
        Direction::Backward => tree::Direction::Backward,
    }
}

impl<S: PageStore + ?Sized> View for PagedView<'_, S> {
    fn version(&self) -> Version {
        self.version
    }

    fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
        let tree = self.tree(path)?;
        if path.is_empty() {
            return Ok(Some(Head {
                list: self.root_has_children(&tree)?,
                value: None,
            }));
        }
        Ok(tree.get(path).map_err(|e| unreadable(path, &e))?.map(head))
    }

    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError> {
        let tree = self.tree(path)?;
        Ok(self.build(&tree, path, limit)?.map(Cow::Owned))
    }

    fn children_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'v>>,
    ) -> Result<bool, ReadError> {
        let tree = self.tree(path)?;
        let mut children = tree.children(path, from, store_direction(direction));
        for given in 0..max {
            match children.next() {
                None => return Ok(false),
                Some(Ok((segment, node))) => out.push(Child {
                    segment: Cow::Owned(segment),
                    head: head(node),
                }),
                // The children before it are given; the next page reaches
                // it first.
                Some(Err(_)) if given > 0 => return Ok(true),
                Some(Err(e)) => return Err(unreadable(path, &e)),
            }
        }
        // A full page: more follow if another child does (one that does
        // not read is for the next page to reach).
        Ok(children.next().is_some())
    }

    fn subtrees_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        limit: usize,
        out: &mut Vec<Cow<'v, OwnedNode>>,
    ) -> Result<bool, ReadError> {
        let tree = self.tree(path)?;
        let mut children = tree.children(path, from, store_direction(direction));
        for given in 0..max {
            let segment = match children.next() {
                None => return Ok(false),
                Some(Ok((segment, _))) => segment,
                Some(Err(_)) if given > 0 => return Ok(true),
                Some(Err(e)) => return Err(unreadable(path, &e)),
            };
            let mut child_path = path.to_vec();
            child_path.push(&segment);
            match self.build(&tree, &child_path, limit) {
                Ok(Some(node)) => out.push(Cow::Owned(node)),
                Ok(None) => return Err(orphan(&child_path)),
                // A subtree past the limit fails the page, as the file
                // engine's view fails it: so would the next page.
                Err(e @ ReadError::TooLarge { .. }) => return Err(e),
                Err(_) if given > 0 => return Ok(true),
                Err(e) => return Err(e),
            }
        }
        Ok(children.next().is_some())
    }

    fn walk_page<'v>(
        &'v self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'v>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        let tree = self.tree(path)?;
        let base = path.len();
        let max = max.max(1);
        let mut given = 0;
        // The key of the last node given, while `have_last`, and the key
        // being read: two buffers for the page, swapped, so that a key lent
        // from its page is copied and never allocated alone.
        let (mut last, mut current) = (Vec::new(), Vec::new());
        let mut have_last = false;
        let start = key::encode(path);
        let end = key::subtree_end(&start);
        let hi = end.as_deref().map_or(Bound::Unbounded, Bound::Excluded);
        let mut items = match after {
            None => {
                if path.is_empty() {
                    out.push(Step {
                        depth: 0,
                        segment: Cow::Owned(Vec::new()),
                        head: Head {
                            list: self.root_has_children(&tree)?,
                            value: None,
                        },
                    });
                    given += 1;
                    have_last = true;
                }
                tree.walk(path)
            }
            Some(after) => {
                let mut from = start.clone();
                for segment in after {
                    key::push_segment(&mut from, segment);
                }
                tree.range(
                    Bound::Excluded(from.as_slice()),
                    hi,
                    tree::Direction::Forward,
                )
            }
        };
        // Where the page resumes: the path of its last node below `path`.
        let resume = |key: &[u8]| -> Vec<Vec<u8>> {
            key::decode(key)
                .map(|mut p| p.split_off(base.min(p.len())))
                .unwrap_or_default()
        };
        loop {
            if given >= max {
                return Ok(have_last.then(|| resume(&last)));
            }
            let lent = items.next_lent(|key, node| {
                current.clear();
                current.extend_from_slice(key);
                let (depth, segment) = last_segment(key);
                (depth, segment, node)
            });
            let (depth, segment, node) = match lent {
                None => return Ok(None),
                Some(Ok(item)) => item,
                Some(Err(_)) if given > 0 => return Ok(have_last.then(|| resume(&last))),
                Some(Err(e)) => return Err(unreadable(path, &e)),
            };
            let Some(below) = depth.checked_sub(base) else {
                return Err(orphan(path));
            };
            if after.is_none() && given == 0 && below != 0 {
                // The walk's own node is missing: nothing below it exists
                // in a tree whose keys make one.
                return Err(orphan(path));
            }
            out.push(Step {
                depth: below,
                segment: Cow::Owned(segment),
                head: head(node),
            });
            given += 1;
            std::mem::swap(&mut last, &mut current);
            have_last = true;
        }
    }

    fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
        self.tree(path)?
            .changed_since(path, since)
            .map_err(|e| unreadable(path, &e))
    }
}

/// How large one batch of an import is at most: one unit of work of the
/// page store each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Batch {
    /// Nodes.
    pub nodes: usize,
    /// Bytes of their paths and values.
    pub bytes: usize,
}

impl Default for Batch {
    fn default() -> Self {
        Batch {
            nodes: 4096,
            bytes: 4 << 20,
        }
    }
}

/// Where an import's batches go: each one entry's unit of work of a page
/// store, committed on its own. The page store that holds the trees
/// implements it once its write path takes imports (the module doc);
/// [`MemStore`] does here, and the tests' page file.
pub trait Units {
    /// Whether a tree named `tree` exists.
    fn has_tree(&self, tree: &TreeName) -> bool;

    /// Apply `edits` to tree `tree` as entry `entry`, in one unit of work,
    /// and commit it: the tree is made when `root` is `None`. Its new root.
    fn apply(
        &mut self,
        tree: &TreeName,
        root: Option<Root>,
        entry: u64,
        edits: &[Edit],
    ) -> Result<Root, tree::Error>;

    /// Drop tree `tree` as entry `entry`, in one unit of work.
    fn drop_tree(&mut self, tree: &TreeName, entry: u64) -> Result<(), tree::Error>;
}

/// [`Units::apply`] on a page store that opens units itself
/// ([`PageStore::begin`]): the unit is opened, the edits applied, the tree
/// named with its root, and the unit committed, or dropped on an error.
pub fn apply_unit<S: PageStore + ?Sized>(
    store: &S,
    tree: &TreeName,
    root: Option<Root>,
    entry: u64,
    edits: &[Edit],
) -> Result<Root, tree::Error> {
    let mut unit = store.begin()?;
    let new = tree::apply(store, &mut unit, root, entry, edits)?;
    match root {
        None => unit.add_tree(tree, new),
        Some(old) if old != new => unit.set_root(tree, new),
        Some(_) => {}
    }
    unit.commit()?;
    Ok(new)
}

/// [`Units::drop_tree`] on a page store that opens units itself.
pub fn drop_unit<S: PageStore + ?Sized>(store: &S, tree: &TreeName) -> Result<(), tree::Error> {
    let mut unit = store.begin()?;
    unit.drop_tree(tree);
    unit.commit()?;
    Ok(())
}

impl Units for MemStore {
    fn has_tree(&self, tree: &TreeName) -> bool {
        self.root(tree).is_some()
    }

    fn apply(
        &mut self,
        tree: &TreeName,
        root: Option<Root>,
        entry: u64,
        edits: &[Edit],
    ) -> Result<Root, tree::Error> {
        apply_unit(&*self, tree, root, entry, edits)
    }

    fn drop_tree(&mut self, tree: &TreeName, _entry: u64) -> Result<(), tree::Error> {
        drop_unit(&*self, tree)
    }
}

/// A file imported ([`import`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    /// The tree's root.
    pub root: Root,
    /// The file's page 0 as read (the module doc, "Page 0").
    pub page_zero: Vec<u8>,
    /// The entries the batches took, the first and the last.
    pub entries: (u64, u64),
    /// The nodes put, the root not counted.
    pub nodes: u64,
}

/// Why an import was refused. The tree it made was dropped again, but for
/// [`ImportError::Left`].
#[derive(Debug)]
pub enum ImportError {
    /// The file does not read as an `.fmp12`: among them an encrypted
    /// file, whose pages do not decode (the module doc).
    File(om_hbam::Error),
    /// The file holds, at `path` (empty for page 0), what an export could
    /// not give back as it is: what `om_hbam`'s writer cannot write, or
    /// writes in another shape (the module doc, "What import accepts").
    Unwritable {
        path: Vec<Vec<u8>>,
        reason: &'static str,
    },
    /// A key at `path` the reader gives as neither a list nor a value,
    /// which the store cannot hold. The reader gives none
    /// (`om_hbam::stream`: a key that is no list has a value).
    NoKind { path: Vec<Vec<u8>> },
    /// An item at `path` past the store's bounds (docs/server-storage.md
    /// 6.11).
    Bounds {
        path: Vec<Vec<u8>>,
        why: tree::Refusal,
    },
    /// The entries a file this large may take, `needed`, do not all fit
    /// between `first` and the last index an entry can have, or `first` is
    /// 0, which is no entry: nothing was written.
    Entries { first: u64, needed: u64 },
    /// A tree of that name exists: nothing was written.
    Exists,
    /// Writing a batch into the page store failed.
    Store(tree::Error),
    /// The import failed with `error`, and dropping the tree it made failed
    /// with `drop`: what its batches wrote is under the tree's name.
    Left {
        error: Box<ImportError>,
        drop: tree::Error,
    },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::File(e) => write!(f, "the file does not read as an .fmp12: {e}"),
            ImportError::Unwritable { path, reason } => write!(
                f,
                "the file holds what an export could not give back at {path:02x?}: {reason}"
            ),
            ImportError::NoKind { path } => {
                write!(f, "a key at {path:02x?} is neither a list nor a value")
            }
            ImportError::Bounds { path, why } => {
                write!(
                    f,
                    "an item at {path:02x?} is past the store's bounds: {why:?}"
                )
            }
            ImportError::Entries { first, needed } => write!(
                f,
                "{needed} entries from entry {first} do not fit below the last entry index"
            ),
            ImportError::Exists => f.write_str("a tree of that name exists"),
            ImportError::Store(e) => write!(f, "the page store refused a batch: {e}"),
            ImportError::Left { error, drop } => write!(
                f,
                "{error}; the tree it made could not be dropped ({drop}) and is left"
            ),
        }
    }
}

impl std::error::Error for ImportError {}

/// Read the `.fmp12` `source` into a new tree named `tree` through `units`,
/// its batches as entries `first_entry`, `first_entry + 1`, ... (the module
/// doc). `first_entry` must be after every entry the page store has, and
/// the entries the file may take must fit below the last index an entry can
/// have ([`ImportError::Entries`], checked before anything is written).
///
/// **What import accepts, export gives back.** For every file it accepts:
///
/// - [`export`] of the tree can be written, for a file whose export numbers
///   fewer than 2^32 pages (the writer's page numbers are 32 bits; an
///   export can number more pages than its source, and a source near the
///   format's 16 TiB may need more than that) and whose change number has
///   the room for them (checked below);
/// - an import of that export holds the same nodes, kinds and values, and,
///   imported at the same entry in the same batches, has the same root
///   hash (a page's hash covers the entry that wrote it, so another first
///   entry or other batches give another root hash over the same nodes);
/// - but for two boundaries of that second import. Page 0's change number:
///   an export advances it by the pages it writes, so the export of a file
///   whose change number is within that many of the bound below may itself
///   be refused. And the entries: their check is worked out from the
///   file's length, and an export can be a page or more longer than its
///   source (a source that leaves out the root's begin, for one), so at a
///   first entry within a few pages' worth of the last index an entry can
///   have, the export's re-import may be refused ([`ImportError::Entries`]).
///   Accepted means exportable once, not exportable and accepted again.
///
/// So it refuses ([`ImportError::Unwritable`]):
///
/// - what the file writer cannot write, and what it writes in another
///   shape: a list that carries an empty value, which it writes as a bare
///   list ([#754](https://github.com/OpenMaker-Dev/openmaker/issues/754)),
///   and a value over [`MAX_INLINE_VALUE`] bytes held whole, which it
///   writes in pieces;
/// - a node whose entry in an export's index could cost more than
///   [`MAX_INDEX_ENTRY`] (1,019 bytes): within that bound the writer's
///   index never passes the 16 levels it writes, whatever else the file
///   holds (`om_hbam::writer::MAX_INDEX_ENTRY` has the derivation);
/// - a page 0 the writer does not write, or whose change number leaves no
///   room for the pages an export numbers from it: at most 2^32, one each,
///   so a change number above `u64::MAX - u32::MAX` is refused whatever the
///   file's size.
pub fn import<R: IoRead + Seek, U: Units + ?Sized>(
    source: R,
    units: &mut U,
    tree: &TreeName,
    first_entry: u64,
    batch: Batch,
) -> Result<Imported, ImportError> {
    if units.has_tree(tree) {
        return Err(ImportError::Exists);
    }
    let mut made = Made {
        root: None,
        next: first_entry,
        last: first_entry,
    };
    match fill(source, units, tree, &mut made, batch) {
        Ok((page_zero, nodes, root)) => Ok(Imported {
            root,
            page_zero,
            entries: (first_entry, made.last),
            nodes,
        }),
        Err(error) if made.root.is_none() => Err(error),
        Err(error) => match units.drop_tree(tree, made.next) {
            Ok(()) => Err(error),
            Err(drop) => Err(ImportError::Left {
                error: Box::new(error),
                drop,
            }),
        },
    }
}

/// What an import made so far: the tree's root once a batch made it, the
/// next entry, and the last one a batch took.
struct Made {
    root: Option<Root>,
    next: u64,
    last: u64,
}

/// The import's work ([`import`]): page 0, the nodes put and the root.
fn fill<R: IoRead + Seek, U: Units + ?Sized>(
    source: R,
    units: &mut U,
    tree: &TreeName,
    made: &mut Made,
    batch: Batch,
) -> Result<(Vec<u8>, u64, Root), ImportError> {
    let mut stream = NodeStream::new(source).map_err(ImportError::File)?;
    let page_zero = stream.superblock().raw.to_vec();
    check_page_zero(&page_zero)?;
    // Every node takes at least one byte of the file, so the file has
    // fewer nodes than bytes, and an import fewer batches than nodes, and
    // one more (a file of no nodes) and the entry that drops the tree.
    let needed = u64::from(stream.page_count())
        .saturating_mul(u64::from(stream.page_size()))
        .saturating_add(2);
    if made.next == 0 || made.next.checked_add(needed).is_none() {
        return Err(ImportError::Entries {
            first: made.next,
            needed,
        });
    }
    let entries = |first: u64| ImportError::Entries { first, needed };
    let mut edits: Vec<Edit> = Vec::new();
    let mut bytes = 0;
    let mut nodes = 0u64;
    let mut flush = |edits: &mut Vec<Edit>, made: &mut Made| -> Result<(), ImportError> {
        let root = units
            .apply(tree, made.root, made.next, edits)
            .map_err(ImportError::Store)?;
        made.root = Some(root);
        made.last = made.next;
        made.next = made.next.checked_add(1).ok_or_else(|| entries(made.next))?;
        edits.clear();
        Ok(())
    };
    while let Some(node) = stream.next_node().map_err(ImportError::File)? {
        let path = node.path.to_vec();
        let kind = match (node.push, node.value) {
            (false, Some(v)) => Node::Value(v.to_vec()),
            (true, None) => Node::List,
            (true, Some(v)) => Node::Both(v.to_vec()),
            (false, None) => return Err(ImportError::NoKind { path }),
        };
        check(&path, &kind)?;
        // The batch's limits are checked before the node joins it: a batch
        // never passes them, but for a node larger than the byte limit
        // alone, which is a batch of its own (the bounds of section 6.11
        // hold a node to 64 KiB of path and 64 KiB of value).
        let size = key::encoded_len(&path) + kind.value().len();
        if !edits.is_empty() && (edits.len() >= batch.nodes.max(1) || bytes + size > batch.bytes) {
            flush(&mut edits, made)?;
            bytes = 0;
        }
        edits.push(Edit::Put { path, node: kind });
        bytes += size;
        nodes += 1;
    }
    if !edits.is_empty() || made.root.is_none() {
        flush(&mut edits, made)?;
    }
    let root = made.root.ok_or(ImportError::Store(tree::Error::Store(
        om_store::page::StoreError::Failed("no batch made the tree".into()),
    )))?;
    Ok((page_zero, nodes, root))
}

/// Refuse a page 0 an export could not write the file with: one that is
/// not one 4,096-byte page, the only page size the file writer writes, or
/// whose change number leaves no room for the pages an export numbers from
/// it (one each; an `.fmp12` has at most 2^32 pages). The room is a worst
/// case: a file whose export numbers far fewer pages is refused too,
/// though the file engine could still save it and the writer write it. The
/// reader checked the rest the writer copies: the magic and the page size
/// it gives.
fn check_page_zero(page_zero: &[u8]) -> Result<(), ImportError> {
    let refuse = |reason| {
        Err(ImportError::Unwritable {
            path: Vec::new(),
            reason,
        })
    };
    if om_hbam::writer::closed_page_zero(page_zero, 0).is_none() {
        return refuse("page 0 is not one 4,096-byte page");
    }
    let room = om_hbam::writer::change_number(page_zero)
        .and_then(|number| number.checked_add(u64::from(u32::MAX)));
    if room.is_none() {
        return refuse("page 0's change number leaves no room for the pages an export numbers");
    }
    Ok(())
}

/// Refuse a node an export could not give back as it is, or the store
/// cannot hold ([`import`], "What import accepts, export gives back").
fn check(path: &[Vec<u8>], node: &Node) -> Result<(), ImportError> {
    if path.is_empty() {
        return Err(ImportError::NoKind { path: Vec::new() });
    }
    if let Err(why) = tree::check_put(path, node) {
        return Err(ImportError::Bounds {
            path: path.to_vec(),
            why,
        });
    }
    let reshaped = match node {
        Node::Both(v) if v.is_empty() => Some(
            "a list that carries an empty value, which the file writer writes as a bare list \
             (#754)",
        ),
        Node::Value(v) if v.len() > MAX_INLINE_VALUE => {
            Some("a value over 1,000 bytes held whole, which the file writer writes in pieces")
        }
        _ => None,
    };
    if let Some(reason) = reshaped {
        return Err(ImportError::Unwritable {
            path: path.to_vec(),
            reason,
        });
    }
    if let Some(refused) = om_hbam::writer::unwritable_key(path, node.is_list(), node.value()) {
        return Err(ImportError::Unwritable {
            path: refused.path,
            reason: refused.reason,
        });
    }
    if index_entry_cost(path, node.is_list()) > MAX_INDEX_ENTRY {
        return Err(ImportError::Unwritable {
            path: path.to_vec(),
            reason: "a path whose entry in an export's index could cost more than 1,019 bytes, \
                     past which the index might not fit in 16 levels",
        });
    }
    Ok(())
}

/// Export the file `view` reads, whose page 0 as imported is `page_zero`,
/// to `target` (`super::export`, written from the view): the walk, written
/// with that page 0 from the change number it holds and checked against a
/// second walk. It is what the file engine's compacted copy of the same
/// file holds. When it fails, what is left under a folder's name is what
/// [`ExportError`] says
/// ([#761](https://github.com/OpenMaker-Dev/openmaker/issues/761)).
pub fn export(
    view: &dyn View,
    page_zero: &[u8],
    target: Target<'_, '_>,
    progress: Progress<'_>,
) -> Result<Exported, ExportError> {
    let change_number = om_hbam::writer::change_number(page_zero).unwrap_or(0);
    export::written(view, page_zero, change_number, target, progress)
}

#[cfg(test)]
mod tests;
