//! Views (docs/server-storage.md 5, "Views"): one fixed version of one
//! file's key tree, for reading.
//!
//! A [`View`] answers a node by its path without what is below it
//! ([`View::node`]), a subtree whole within a limit the caller states
//! ([`View::subtree`]), the children of a node in key order from a given
//! segment, in either direction, a page at a time ([`View::children_page`],
//! [`View::subtrees_page`]), and a depth-first walk of a subtree, a page at
//! a time ([`View::walk_page`]). The pages are read through iterators that
//! ask for the next page when they need it ([`Children`], [`Subtrees`],
//! [`Walk`]), so that a reader of a long list (a table's records, an index)
//! never holds it whole, and makes one call through the trait per page, not
//! per node. Every read returns a [`Result`]: the file engine's never fail,
//! another engine's fail where what it reads is damaged (section 6.12).
//!
//! # Borrowing
//!
//! What a read returns borrows the view (`Cow<'v, ...>` for `&'v self`): it
//! never outlives the view, and a reader that keeps something past the
//! view copies it (`into_owned`). The file engine lends its tree (the read
//! lock the view holds keeps it there), so it copies nothing a reader does
//! not ask to own. An engine that reads pages on demand gives owned values:
//! it holds a page only while a call reads it, never for as long as the
//! view lives. A page's items come back owned or borrowed as the engine
//! chooses; readers take either.
//!
//! # Limits
//!
//! [`View::subtree`] reads a subtree whole and refuses one larger than the
//! limit its caller states ([`ReadError::TooLarge`]), counted as an owned
//! tree's memory ([`om_hbam::tree::memory`]), so that no reader pulls a
//! table into memory by accident. [`NO_LIMIT`] says that the caller holds
//! a subtree whole whatever its size, as the readers moved onto views from
//! the store did before; docs/server-storage.md 5 lists them.
//!
//! The file engine's view is [`TreeView`] over its store's tree. Its
//! version counts the times the store was taken for writing, and it
//! answers [`View::changed_since`] for the whole file: whether any write
//! came since.

use std::borrow::Cow;
use std::fmt;
use std::ops::Bound;

use om_hbam::OwnedNode;

/// A version of a file's tree: the views of one version read the same, and
/// a later version has a larger number. Versions compare only within one
/// load of a file (the file engine's store counts from 0 again when a file
/// is opened again).
pub type Version = u64;

/// The limit of [`View::subtree`] for a reader that holds the subtree
/// whole whatever its size (the module doc, "Limits").
pub const NO_LIMIT: usize = usize::MAX;

/// How many children or nodes of a walk the iterators ask a view for at
/// once. A view may answer fewer.
const PAGE: usize = 256;

/// A subtree a view gave, held while it is read: lent (the file engine's
/// view lends its tree) it is left alone; a copy the view made (a view
/// that reads pages, the test view) is wiped when dropped, for it holds
/// secrets (an account's password verifier, a stored password).
pub(crate) struct Wiped<'v>(pub Cow<'v, OwnedNode>);

impl Drop for Wiped<'_> {
    fn drop(&mut self) {
        /// Every value of `node`, wiped. Bounded: a node of the file is at
        /// most 17 keys deep (`om_hbam::slot::MAX_DEPTH`).
        fn wipe(node: &mut OwnedNode) {
            use zeroize::Zeroize;
            if let Some(value) = node.value.as_mut() {
                value.zeroize();
            }
            for child in &mut node.children {
                wipe(child);
            }
        }
        if let Cow::Owned(node) = &mut self.0 {
            wipe(node);
        }
    }
}

/// One node of a view, without its children.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head<'v> {
    /// Whether the node opens a list: it was written as a list (a push,
    /// possibly empty), or it has children. A node that does not is a value.
    /// A node that opens a list and holds a value is both
    /// (docs/server-storage.md 4).
    pub list: bool,
    /// The node's own value, if it has one. A value too large for one chunk
    /// is split into pieces that are the node's children (file-format.md
    /// 3.6); this is the node's own, not the pieces joined.
    pub value: Option<Cow<'v, [u8]>>,
}

impl Head<'_> {
    pub(super) fn of(node: &OwnedNode) -> Head<'_> {
        Head {
            list: node.push || !node.children.is_empty(),
            value: node.value.as_deref().map(Cow::Borrowed),
        }
    }

    /// The same node, owning its value (the test view's, `super::copies`).
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub fn into_owned(self) -> Head<'static> {
        Head {
            list: self.list,
            value: self.value.map(|value| Cow::Owned(value.into_owned())),
        }
    }
}

/// A child of a node: its segment and its own node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Child<'v> {
    pub segment: Cow<'v, [u8]>,
    pub head: Head<'v>,
}

/// One node of a depth-first walk ([`View::walk_page`]): its depth below
/// the node the walk started at (0 for that node itself), its segment and
/// its own node. A node comes after its parent and before its next
/// sibling, siblings in key order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step<'v> {
    pub depth: usize,
    pub segment: Cow<'v, [u8]>,
    pub head: Head<'v>,
}

/// Which way children are listed: in key order, or against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Forward,
    #[allow(
        dead_code,
        reason = "part of a view's reads (docs/server-storage.md 5); no reader moved onto views \
                  lists backward yet"
    )]
    Backward,
}

/// Why a read failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The engine could not read part of the version: its storage failed,
    /// or found what it read damaged (docs/server-storage.md 6.12). The
    /// file engine never answers this.
    Unreadable { path: Vec<Vec<u8>>, why: String },
    /// The subtree at `path`, asked for whole, takes more memory than the
    /// caller's limit ([`View::subtree`]).
    TooLarge { path: Vec<Vec<u8>>, limit: usize },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Unreadable { path, why } => {
                write!(f, "{path:02x?} could not be read: {why}")
            }
            ReadError::TooLarge { path, limit } => {
                write!(f, "{path:02x?} holds more than {limit} bytes asked for")
            }
        }
    }
}

impl std::error::Error for ReadError {}

/// A read error as the `.fmp12` reader's: what an `om_query::Source`
/// answers. A failed read is an I/O error (reading the file failed), a
/// subtree past its limit the reader's own limit error.
impl From<ReadError> for om_hbam::Error {
    fn from(error: ReadError) -> Self {
        match error {
            ReadError::TooLarge { limit, .. } => om_hbam::Error::TooLarge { limit },
            error @ ReadError::Unreadable { .. } => {
                om_hbam::Error::Io(std::io::Error::other(error))
            }
        }
    }
}

/// `path` as owned segments, for an error.
pub fn owned_path(path: &[&[u8]]) -> Vec<Vec<u8>> {
    path.iter().map(|segment| segment.to_vec()).collect()
}

/// One fixed version of one file's tree (the module doc). Nothing it
/// returns changes later.
pub trait View {
    /// The version this view reads.
    fn version(&self) -> Version;

    /// The node at `path` (the empty path is the root), without its
    /// children; `None` when there is none.
    fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError>;

    /// The node at `path` and everything below it, as an owned tree's form:
    /// its segment is `path`'s last (empty for the root). Refused with
    /// [`ReadError::TooLarge`] when it would take more than `limit` bytes as
    /// an owned tree ([`om_hbam::tree::memory`]); [`NO_LIMIT`] for none.
    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError>;

    /// Up to `max` children of the node at `path`, from `from` in
    /// `direction`, onto `out`: forward, those at or after an included
    /// segment (after an excluded one, from the first when unbounded);
    /// backward, those at or before it (before an excluded one, from the
    /// last when unbounded). Whether more children may follow the last one
    /// given: a view may give fewer than `max` and still have more. A
    /// missing node has no children.
    fn children_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'v>>,
    ) -> Result<bool, ReadError>;

    /// [`View::children_page`], each child with its subtree, refused as
    /// [`View::subtree`] refuses when one takes more than `limit`.
    #[allow(clippy::too_many_arguments)]
    fn subtrees_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        limit: usize,
        out: &mut Vec<Cow<'v, OwnedNode>>,
    ) -> Result<bool, ReadError>;

    /// Up to `max` nodes (at least one) of a depth-first walk of the
    /// subtree at `path` onto `out`: from the node at `path` itself, or after the node at
    /// `after`, a path relative to `path` that an earlier page of the same
    /// walk ended with. The path of the last node given when more may
    /// follow, `None` when the walk is done (and for a missing node).
    fn walk_page<'v>(
        &'v self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'v>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError>;

    /// Whether anything at or below `path` changed after version `since`.
    /// Never false when something did; it may be true when nothing did.
    fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError>;

    /// How many children the node at `path` has; 0 for a missing node.
    /// Counted by listing them ([`View::children_page`]) unless the view
    /// knows the count, as the file engine's tree does.
    fn count_children(&self, path: &[&[u8]]) -> Result<usize, ReadError> {
        let mut count = 0;
        let mut after: Option<Vec<u8>> = None;
        let mut page = Vec::new();
        loop {
            page.clear();
            let from = after.as_deref().map_or(Bound::Unbounded, Bound::Excluded);
            let more = self.children_page(path, from, Direction::Forward, PAGE, &mut page)?;
            count += page.len();
            match page.last() {
                Some(last) if more => after = Some(last.segment.to_vec()),
                _ => return Ok(count),
            }
        }
    }
}

/// A view over another that keeps the first of its reads that failed, for
/// a reader that reads through an interface with no room for the view's
/// error (a query's source, whose errors are the file's:
/// `om_query::Source`), so that it can tell a failed read from what it
/// read ([`Noted::failed`]).
pub struct Noted<'v> {
    inner: &'v dyn View,
    failed: std::cell::RefCell<Option<ReadError>>,
}

impl<'v> Noted<'v> {
    pub fn new(inner: &'v dyn View) -> Self {
        Noted {
            inner,
            failed: std::cell::RefCell::new(None),
        }
    }

    /// The first read that failed, if one did.
    pub fn failed(&self) -> Result<(), ReadError> {
        match self.failed.borrow().as_ref() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn note<T>(&self, read: Result<T, ReadError>) -> Result<T, ReadError> {
        if let Err(error) = &read {
            self.failed
                .borrow_mut()
                .get_or_insert_with(|| error.clone());
        }
        read
    }
}

impl View for Noted<'_> {
    fn version(&self) -> Version {
        self.inner.version()
    }

    fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
        self.note(self.inner.node(path))
    }

    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError> {
        self.note(self.inner.subtree(path, limit))
    }

    fn children_page<'a>(
        &'a self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'a>>,
    ) -> Result<bool, ReadError> {
        self.note(self.inner.children_page(path, from, direction, max, out))
    }

    fn subtrees_page<'a>(
        &'a self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        limit: usize,
        out: &mut Vec<Cow<'a, OwnedNode>>,
    ) -> Result<bool, ReadError> {
        self.note(
            self.inner
                .subtrees_page(path, from, direction, max, limit, out),
        )
    }

    fn walk_page<'a>(
        &'a self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'a>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        self.note(self.inner.walk_page(path, after, max, out))
    }

    fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
        self.note(self.inner.changed_since(path, since))
    }

    fn count_children(&self, path: &[&[u8]]) -> Result<usize, ReadError> {
        self.note(self.inner.count_children(path))
    }
}

impl<'v> dyn View + 'v {
    /// The children of the node at `path`, from `from` in `direction`
    /// ([`View::children_page`]), read a page at a time as the iterator
    /// goes. After an error it ends.
    pub fn children(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
    ) -> Children<'v> {
        Children {
            view: self,
            path: owned_path(path),
            next: Some(owned_bound(from)),
            direction,
            page: Vec::new(),
        }
    }

    /// The children of the node at `path` with their subtrees, each within
    /// `limit` ([`View::subtrees_page`]), read a page at a time.
    pub fn subtrees(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        limit: usize,
    ) -> Subtrees<'v> {
        Subtrees {
            view: self,
            path: owned_path(path),
            next: Some(owned_bound(from)),
            direction,
            limit,
            page: Vec::new(),
        }
    }

    /// A depth-first walk of the subtree at `path` ([`View::walk_page`]),
    /// read a page at a time.
    pub fn walk(&'v self, path: &[&[u8]]) -> Walk<'v> {
        Walk {
            view: self,
            path: owned_path(path),
            next: Some(None),
            page: Vec::new(),
        }
    }
}

fn owned_bound(bound: Bound<&[u8]>) -> Bound<Vec<u8>> {
    bound.map(<[u8]>::to_vec)
}

/// The children of a node, a page at a time ([`View::children`]).
pub struct Children<'v> {
    view: &'v dyn View,
    path: Vec<Vec<u8>>,
    /// Where the next page starts; `None` when there is none.
    next: Option<Bound<Vec<u8>>>,
    direction: Direction,
    /// The page being given, its last item first: one buffer for every
    /// page.
    page: Vec<Child<'v>>,
}

impl<'v> Iterator for Children<'v> {
    type Item = Result<Child<'v>, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(child) = self.page.pop() {
                return Some(Ok(child));
            }
            let from = self.next.take()?;
            let path: Vec<&[u8]> = self.path.iter().map(Vec::as_slice).collect();
            let from_ref = from.as_ref().map(Vec::as_slice);
            match self
                .view
                .children_page(&path, from_ref, self.direction, PAGE, &mut self.page)
            {
                Err(error) => {
                    self.page.clear();
                    return Some(Err(error));
                }
                Ok(more) => {
                    if more {
                        // A view that says more may follow gave at least one.
                        self.next = self
                            .page
                            .last()
                            .map(|child| Bound::Excluded(child.segment.to_vec()));
                    }
                }
            }
            self.page.reverse();
        }
    }
}

/// The children of a node with their subtrees, a page at a time
/// ([`View::subtrees`]).
pub struct Subtrees<'v> {
    view: &'v dyn View,
    path: Vec<Vec<u8>>,
    next: Option<Bound<Vec<u8>>>,
    direction: Direction,
    limit: usize,
    /// The page being given, its last item first.
    page: Vec<Cow<'v, OwnedNode>>,
}

impl<'v> Iterator for Subtrees<'v> {
    type Item = Result<Cow<'v, OwnedNode>, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(child) = self.page.pop() {
                return Some(Ok(child));
            }
            let from = self.next.take()?;
            let path: Vec<&[u8]> = self.path.iter().map(Vec::as_slice).collect();
            let from_ref = from.as_ref().map(Vec::as_slice);
            match self.view.subtrees_page(
                &path,
                from_ref,
                self.direction,
                PAGE,
                self.limit,
                &mut self.page,
            ) {
                Err(error) => {
                    self.page.clear();
                    return Some(Err(error));
                }
                Ok(more) => {
                    if more {
                        self.next = self
                            .page
                            .last()
                            .map(|child| Bound::Excluded(child.segment.clone()));
                    }
                }
            }
            self.page.reverse();
        }
    }
}

/// A depth-first walk of a subtree, a page at a time ([`View::walk`]).
pub struct Walk<'v> {
    view: &'v dyn View,
    path: Vec<Vec<u8>>,
    /// Where the next page starts: from the start (`Some(None)`), after a
    /// node, or nowhere (`None`).
    next: Option<Option<Vec<Vec<u8>>>>,
    /// The page being given, its last item first.
    page: Vec<Step<'v>>,
}

impl<'v> Iterator for Walk<'v> {
    type Item = Result<Step<'v>, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(step) = self.page.pop() {
                return Some(Ok(step));
            }
            let after = self.next.take()?;
            let path: Vec<&[u8]> = self.path.iter().map(Vec::as_slice).collect();
            match self
                .view
                .walk_page(&path, after.as_deref(), PAGE, &mut self.page)
            {
                Err(error) => {
                    self.page.clear();
                    return Some(Err(error));
                }
                Ok(resume) => self.next = resume.map(Some),
            }
            self.page.reverse();
        }
    }
}

/// The file engine's view: a tree in memory, lent as it is. Its reads never
/// fail, and [`View::changed_since`] answers for the whole file.
#[derive(Clone, Copy)]
pub struct TreeView<'t> {
    root: &'t OwnedNode,
    version: Version,
}

impl<'t> TreeView<'t> {
    /// The tree `root` at `version`.
    pub fn new(root: &'t OwnedNode, version: Version) -> Self {
        TreeView { root, version }
    }
}

/// The position among `node`'s children where a listing from `from` in
/// `direction` starts: the index of the first child to give forward, one
/// past it backward.
fn start(node: &OwnedNode, from: Bound<&[u8]>, direction: Direction) -> usize {
    let at = |segment: &[u8]| {
        node.children
            .binary_search_by(|child| child.segment.as_slice().cmp(segment))
    };
    match (direction, from) {
        (Direction::Forward, Bound::Unbounded) => 0,
        (Direction::Forward, Bound::Included(s)) => at(s).unwrap_or_else(|i| i),
        (Direction::Forward, Bound::Excluded(s)) => at(s).map_or_else(|i| i, |i| i + 1),
        (Direction::Backward, Bound::Unbounded) => node.children.len(),
        (Direction::Backward, Bound::Included(s)) => at(s).map_or_else(|i| i, |i| i + 1),
        (Direction::Backward, Bound::Excluded(s)) => at(s).unwrap_or_else(|i| i),
    }
}

/// Up to `max` of `node`'s children from `from` in `direction`, each given
/// to `take`; whether more follow.
pub(super) fn page_of<'t>(
    node: &'t OwnedNode,
    from: Bound<&[u8]>,
    direction: Direction,
    max: usize,
    mut take: impl FnMut(&'t OwnedNode) -> Result<(), ReadError>,
) -> Result<bool, ReadError> {
    let start = start(node, from, direction);
    match direction {
        Direction::Forward => {
            let rest = &node.children[start..];
            for child in rest.iter().take(max) {
                take(child)?;
            }
            Ok(rest.len() > max)
        }
        Direction::Backward => {
            let rest = &node.children[..start];
            for child in rest.iter().rev().take(max) {
                take(child)?;
            }
            Ok(rest.len() > max)
        }
    }
}

/// `node`, at `path`, if it takes at most `limit` as an owned tree.
pub(super) fn within(
    node: &OwnedNode,
    path: impl FnOnce() -> Vec<Vec<u8>>,
    limit: usize,
) -> Result<&OwnedNode, ReadError> {
    if limit == NO_LIMIT || om_hbam::tree::memory_within(node, limit).is_some() {
        Ok(node)
    } else {
        Err(ReadError::TooLarge {
            path: path(),
            limit,
        })
    }
}

impl<'t> TreeView<'t> {
    /// [`View::node`], lent for as long as the tree.
    pub fn head(&self, path: &[&[u8]]) -> Option<Head<'t>> {
        self.root.at(path).map(Head::of)
    }

    /// [`View::subtree`], lent for as long as the tree.
    pub fn lend(&self, path: &[&[u8]], limit: usize) -> Result<Option<&'t OwnedNode>, ReadError> {
        match self.root.at(path) {
            None => Ok(None),
            Some(node) => within(node, || owned_path(path), limit).map(Some),
        }
    }

    pub(crate) fn children_into(
        &self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'t>>,
    ) -> Result<bool, ReadError> {
        let Some(node) = self.root.at(path) else {
            return Ok(false);
        };
        page_of(node, from, direction, max, |child| {
            out.push(Child {
                segment: Cow::Borrowed(&child.segment),
                head: Head::of(child),
            });
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn subtrees_into(
        &self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        limit: usize,
        out: &mut Vec<Cow<'t, OwnedNode>>,
    ) -> Result<bool, ReadError> {
        let Some(node) = self.root.at(path) else {
            return Ok(false);
        };
        page_of(node, from, direction, max, |child| {
            let path = || {
                let mut path = owned_path(path);
                path.push(child.segment.clone());
                path
            };
            out.push(Cow::Borrowed(within(child, path, limit)?));
            Ok(())
        })
    }

    pub(crate) fn walk_into(
        &self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'t>>,
    ) -> Option<Vec<Vec<u8>>> {
        let top = self.root.at(path)?;
        // Each node whose children are being walked, with the position of
        // the next child to give.
        let mut stack: Vec<(&'t OwnedNode, usize)> = Vec::new();
        let mut given = 0;
        match after {
            None => {
                out.push(Step {
                    depth: 0,
                    segment: Cow::Borrowed(&top.segment),
                    head: Head::of(top),
                });
                given += 1;
                stack.push((top, 0));
            }
            Some(after) => {
                // Back to where the last page ended: below each node on the
                // way, the children after the one the path goes through,
                // and below the last node its own children.
                let mut node = top;
                stack.push((top, 0));
                for segment in after {
                    let at = node
                        .children
                        .binary_search_by(|child| child.segment.as_slice().cmp(segment));
                    let last = stack.last_mut().expect("the walk's own node");
                    match at {
                        Ok(i) => {
                            last.1 = i + 1;
                            node = &node.children[i];
                            stack.push((node, 0));
                        }
                        // Not in this version: the walk goes on after where
                        // it would be.
                        Err(i) => {
                            last.1 = i;
                            break;
                        }
                    }
                }
            }
        }
        loop {
            if given >= max {
                // The page is full. It ended with the node on top of the
                // stack, which the next page resumes after: checked before
                // a node is popped, so that the path is that node's, not
                // its parent's (whose children would then be given again).
                return Some(
                    stack
                        .iter()
                        .skip(1)
                        .map(|(node, _)| node.segment.clone())
                        .collect(),
                );
            }
            let (node, next) = stack.last_mut()?;
            let Some(child) = node.children.get(*next) else {
                stack.pop();
                continue;
            };
            *next += 1;
            let depth = stack.len();
            out.push(Step {
                depth,
                segment: Cow::Borrowed(&child.segment),
                head: Head::of(child),
            });
            given += 1;
            stack.push((child, 0));
        }
    }
}

impl View for TreeView<'_> {
    fn version(&self) -> Version {
        self.version
    }

    fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
        Ok(self.head(path))
    }

    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError> {
        Ok(self.lend(path, limit)?.map(Cow::Borrowed))
    }

    fn children_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'v>>,
    ) -> Result<bool, ReadError> {
        self.children_into(path, from, direction, max, out)
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
        self.subtrees_into(path, from, direction, max, limit, out)
    }

    fn walk_page<'v>(
        &'v self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'v>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        Ok(self.walk_into(path, after, max, out))
    }

    fn changed_since(&self, _path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
        Ok(self.version > since)
    }

    fn count_children(&self, path: &[&[u8]]) -> Result<usize, ReadError> {
        Ok(self.root.at(path).map_or(0, |node| node.children.len()))
    }
}

/// The wire form of the subtree at `path` ([`om_hbam::Node::to_wire`]),
/// written as the view walks it, so that it is never held as a tree:
/// `None` when the node is missing, an empty stream when a key or value
/// cannot be written (as `to_wire().unwrap_or_default()` gives).
pub fn wire(view: &dyn View, path: &[&[u8]]) -> Result<Option<Vec<u8>>, ReadError> {
    let mut writer = om_hbam::tree::WireWriter::new();
    let mut found = false;
    for step in view.walk(path) {
        let step = step?;
        found = true;
        if step.depth > 0 {
            writer.node(
                step.depth,
                &step.segment,
                step.head.list,
                step.head.value.as_deref(),
            );
        }
    }
    Ok(found.then(|| writer.finish().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How long a download of a table's records holds the store's read
    /// lock: before views, while the subtree was copied (`read_node`, the
    /// copy written in the wire form after the lock was let go); now, while
    /// it is written in the wire form from a walk of the view. A table of
    /// `OM_VIEW_RECORDS` (200,000) records of three text cells, as
    /// `tests/view_timings.rs` generates; the median of 9 runs. For release
    /// builds:
    ///
    /// ```sh
    /// cargo test --release -p om-server --lib -- --ignored download_hold --nocapture
    /// ```
    #[test]
    #[ignore = "a timing for release builds"]
    fn download_hold() {
        use std::time::{Duration, Instant};
        let count: u64 = std::env::var("OM_VIEW_RECORDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(200_000);
        let cell = |field: u8, text: &str| OwnedNode::leaf([field], om_scsu::encode_scsu(text));
        let records = (1..=count)
            .map(|id| {
                let name = format!("Task {id:06} of the generated file, for timing reads");
                let n = id % 1000;
                let cells = vec![
                    cell(1, &name),
                    cell(2, &n.to_string()),
                    cell(5, &(2 * n).to_string()),
                ];
                OwnedNode::list(om_hbam::serial::encode(id), cells)
            })
            .collect();
        let mut root = OwnedNode::default();
        root.put(&[&[0x80, 0x01], &[5]], OwnedNode::list(vec![5], records));
        let view = TreeView::new(&root, 0);
        let path: [&[u8]; 2] = [&[0x80, 0x01], &[5]];
        let median = |mut runs: Vec<Duration>| {
            runs.sort();
            runs[runs.len() / 2].as_secs_f64() * 1000.0
        };
        let (mut copies, mut writes) = (Vec::new(), Vec::new());
        for _ in 0..9 {
            let started = Instant::now();
            let copy = view.subtree(&path, NO_LIMIT).unwrap().unwrap().into_owned();
            copies.push(started.elapsed());
            let started = Instant::now();
            let stream = wire(&view, &path).unwrap().unwrap();
            writes.push(started.elapsed());
            assert_eq!(Some(stream), copy.to_wire());
        }
        println!(
            "{count} records: copy {:.1} ms, wire form from a walk {:.1} ms",
            median(copies),
            median(writes)
        );
    }

    /// A tree with a list, a value, an empty list, a node both and a deep
    /// path.
    fn tree() -> OwnedNode {
        let mut root = OwnedNode::default();
        for id in 0..10u8 {
            root.put(&[&[1], &[id]], OwnedNode::leaf(Vec::new(), vec![id; 3]));
        }
        root.put(&[&[2]], OwnedNode::leaf(Vec::new(), b"value".to_vec()));
        root.put(&[&[3]], OwnedNode::list(Vec::new(), Vec::new()));
        let mut both = OwnedNode::list(Vec::new(), vec![OwnedNode::leaf([7], [8])]);
        both.value = Some(b"both".to_vec());
        root.put(&[&[4]], both);
        root.put(
            &[&[5], &[6], &[7], &[8]],
            OwnedNode::leaf(Vec::new(), b"deep".to_vec()),
        );
        root
    }

    /// Every way of reading a view gives what the tree holds: children both
    /// ways from every bound, with pages of every size; walks whose pages
    /// end anywhere; subtrees within and past a limit; and the wire form a
    /// walk writes is `to_wire`'s.
    #[test]
    fn a_tree_view_reads_the_tree_in_pages() {
        let root = tree();
        let view = TreeView::new(&root, 1);
        let dynamic: &dyn View = &view;
        for direction in [Direction::Forward, Direction::Backward] {
            for from in [None, Some(0u8), Some(4), Some(9), Some(12)] {
                for included in [true, false] {
                    let key = from.map(|f| vec![f]);
                    let bound = match &key {
                        None => Bound::Unbounded,
                        Some(k) if included => Bound::Included(k.as_slice()),
                        Some(k) => Bound::Excluded(k.as_slice()),
                    };
                    let mut expected: Vec<u8> = (0..10u8)
                        .filter(|id| match (direction, bound) {
                            (_, Bound::Unbounded) => true,
                            (Direction::Forward, Bound::Included(k)) => &[*id][..] >= k,
                            (Direction::Forward, Bound::Excluded(k)) => &[*id][..] > k,
                            (Direction::Backward, Bound::Included(k)) => &[*id][..] <= k,
                            (Direction::Backward, Bound::Excluded(k)) => &[*id][..] < k,
                        })
                        .collect();
                    if direction == Direction::Backward {
                        expected.reverse();
                    }
                    let listed: Vec<u8> = dynamic
                        .children(&[&[1]], bound, direction)
                        .map(|child| child.unwrap().segment[0])
                        .collect();
                    assert_eq!(listed, expected, "{direction:?} {bound:?}");
                    let whole: Vec<u8> = dynamic
                        .subtrees(&[&[1]], bound, direction, NO_LIMIT)
                        .map(|child| child.unwrap().value.clone().unwrap()[0])
                        .collect();
                    assert_eq!(whole, expected, "{direction:?} {bound:?}");
                    // Pages of one and of three give the same.
                    for max in [1, 3] {
                        let mut out = Vec::new();
                        let mut next = bound.map(<[u8]>::to_vec);
                        loop {
                            let before = out.len();
                            let more = view
                                .children_page(
                                    &[&[1]],
                                    next.as_ref().map(Vec::as_slice),
                                    direction,
                                    max,
                                    &mut out,
                                )
                                .unwrap();
                            assert!(out.len() - before <= max);
                            if !more {
                                break;
                            }
                            next = Bound::Excluded(out.last().unwrap().segment.to_vec());
                        }
                        let paged: Vec<u8> = out.iter().map(|c| c.segment[0]).collect();
                        assert_eq!(paged, expected);
                    }
                }
            }
        }
        // A walk in pages of every size is one walk.
        let mut whole = Vec::new();
        assert_eq!(
            view.walk_page(&[], None, usize::MAX, &mut whole).unwrap(),
            None
        );
        assert_eq!(whole.len(), 1 + 11 + 1 + 1 + 2 + 4);
        for max in 1..whole.len() + 2 {
            let mut out = Vec::new();
            let mut after = None;
            loop {
                let resume = view
                    .walk_page(&[], after.as_deref(), max, &mut out)
                    .unwrap();
                match resume {
                    Some(resume) => after = Some(resume),
                    None => break,
                }
            }
            assert_eq!(out, whole, "pages of {max}");
        }
        let iterated: Vec<Step> = dynamic.walk(&[]).map(Result::unwrap).collect();
        assert_eq!(iterated, whole);
        // The wire form of each subtree, written from a walk.
        for path in [&[][..], &[&[1][..]], &[&[4][..]], &[&[5][..]], &[&[2][..]]] {
            let node = root.at(path).unwrap();
            assert_eq!(
                wire(dynamic, path).unwrap(),
                Some(node.to_wire().unwrap()),
                "{path:02x?}"
            );
        }
        assert_eq!(wire(dynamic, &[&[9]]).unwrap(), None);
        // Heads, subtrees and limits.
        assert_eq!(
            view.node(&[&[4]]).unwrap(),
            Some(Head {
                list: true,
                value: Some(Cow::Borrowed(&b"both"[..]))
            })
        );
        assert!(!view.node(&[&[2]]).unwrap().unwrap().list);
        assert!(view.node(&[&[3]]).unwrap().unwrap().list);
        assert_eq!(view.node(&[&[9]]).unwrap(), None);
        let list = root.at(&[&[1]]).unwrap();
        let held = om_hbam::tree::memory(list);
        assert!(matches!(
            view.subtree(&[&[1]], held).unwrap(),
            Some(Cow::Borrowed(node)) if node == list
        ));
        assert_eq!(
            view.subtree(&[&[1]], held - 1),
            Err(ReadError::TooLarge {
                path: vec![vec![1]],
                limit: held - 1
            })
        );
        assert!(view.changed_since(&[], 0).unwrap());
        assert!(!view.changed_since(&[], 1).unwrap());
    }
}
