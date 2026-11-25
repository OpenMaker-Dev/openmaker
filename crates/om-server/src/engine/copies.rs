//! A second view of a hosted file, for tests only (debug builds): it shows
//! that readers do not lean on the file engine's (docs/server-storage.md
//! 5, "Views"). [`Copies`] serves the same tree, but hands out every node
//! as an owned copy, gives children and walks three at a time, and fails
//! every read that reaches a path it was told to fail, as an engine fails
//! the reads that reach a damaged page (section 6.12): the node at that
//! path and everything below it.
//!
//! A server switched to it (`Server::checking_reads`) answers each read
//! that goes through [`super::Reading::serve`] (downloads and finds)
//! through the file engine's view first and then through this one, from
//! the same version, and panics when the two answers differ: the test
//! fixtures of `tests/` serve every request so, and a difference fails the
//! test that made it. Told to fail a path (`ServerHandle::failing_reads`),
//! it answers through this view alone, so that the client gets what a
//! failed read gives. Its derived state is computed through it, as another
//! engine would compute its own, and checked against the file engine's.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Bound;
use std::sync::{Arc, Mutex, PoisonError};

use log::warn;
use om_hbam::OwnedNode;

use super::reading::{Read, Reading};
use super::view::{Child, Direction, Head, ReadError, Step, Version, View};
use crate::derived::{self, Derived};

/// The most children or nodes of a walk one call gives.
const PAGE: usize = 3;

/// A view over another that copies everything it gives, in small pages,
/// and fails the reads that reach `failing`.
pub(crate) struct Copies<'v> {
    inner: &'v dyn View,
    failing: &'v [Vec<Vec<u8>>],
}

impl<'v> Copies<'v> {
    pub(crate) fn new(inner: &'v dyn View, failing: &'v [Vec<Vec<u8>>]) -> Self {
        Copies { inner, failing }
    }

    /// The failing path at or above `path`, if any.
    fn failing_at(&self, path: &[&[u8]]) -> Option<&Vec<Vec<u8>>> {
        self.failing.iter().find(|failing| {
            failing.len() <= path.len() && failing.iter().zip(path).all(|(f, p)| f == p)
        })
    }

    /// Fail a read of the node at `path`.
    fn check(&self, path: &[&[u8]]) -> Result<(), ReadError> {
        match self.failing_at(path) {
            Some(failing) => Err(unreadable(failing)),
            None => Ok(()),
        }
    }

    /// Fail a read of the subtree at `path`: at or below a failing path, or
    /// holding one that exists.
    fn check_subtree(&self, path: &[&[u8]]) -> Result<(), ReadError> {
        self.check(path)?;
        for failing in self.failing {
            let below = failing.len() > path.len() && failing.iter().zip(path).all(|(f, p)| f == p);
            let segments: Vec<&[u8]> = failing.iter().map(Vec::as_slice).collect();
            if below && self.inner.node(&segments)?.is_some() {
                return Err(unreadable(failing));
            }
        }
        Ok(())
    }
}

fn unreadable(path: &[Vec<u8>]) -> ReadError {
    ReadError::Unreadable {
        path: path.to_vec(),
        why: "the test view was told to fail it".into(),
    }
}

fn child_path<'p>(path: &[&'p [u8]], segment: &'p [u8]) -> Vec<&'p [u8]> {
    let mut path = path.to_vec();
    path.push(segment);
    path
}

impl View for Copies<'_> {
    fn version(&self) -> Version {
        self.inner.version()
    }

    fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
        self.check(path)?;
        Ok(self.inner.node(path)?.map(Head::into_owned))
    }

    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError> {
        self.check_subtree(path)?;
        Ok(self
            .inner
            .subtree(path, limit)?
            .map(|node| Cow::Owned(node.into_owned())))
    }

    fn children_page<'a>(
        &'a self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'a>>,
    ) -> Result<bool, ReadError> {
        // Listing a node's children reads where the node is, whether it is
        // there or not.
        self.check(path)?;
        let mut page = Vec::new();
        let more = self
            .inner
            .children_page(path, from, direction, max.min(PAGE), &mut page)?;
        for (given, child) in page.into_iter().enumerate() {
            if let Err(error) = self.check(&child_path(path, &child.segment)) {
                // The children before it are given; the next page reaches
                // it first.
                return if given > 0 { Ok(true) } else { Err(error) };
            }
            out.push(Child {
                segment: Cow::Owned(child.segment.into_owned()),
                head: child.head.into_owned(),
            });
        }
        Ok(more)
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
        self.check(path)?;
        let mut page = Vec::new();
        let more =
            self.inner
                .subtrees_page(path, from, direction, max.min(PAGE), limit, &mut page)?;
        for (given, child) in page.into_iter().enumerate() {
            if let Err(error) = self.check_subtree(&child_path(path, &child.segment)) {
                return if given > 0 { Ok(true) } else { Err(error) };
            }
            out.push(Cow::Owned(child.into_owned()));
        }
        Ok(more)
    }

    fn walk_page<'a>(
        &'a self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'a>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        // A walk reads where its node is, whether it is there or not, and
        // a page resumed after a node reads where that node is.
        self.check(path)?;
        if let Some(after) = after {
            let mut at = path.to_vec();
            at.extend(after.iter().map(Vec::as_slice));
            self.check(&at)?;
        }
        let mut page = Vec::new();
        let resume = self
            .inner
            .walk_page(path, after, max.min(PAGE), &mut page)?;
        // Each step's path below `path`, followed from where the page
        // starts, and that of the last step given.
        let mut below: Vec<Vec<u8>> = after.map(<[Vec<u8>]>::to_vec).unwrap_or_default();
        let mut last = None;
        for step in page {
            below.truncate(step.depth.saturating_sub(1));
            if step.depth > 0 {
                below.push(step.segment.to_vec());
            }
            let mut at = path.to_vec();
            at.extend(below.iter().map(Vec::as_slice));
            if let Err(error) = self.check(&at) {
                // The steps before it are given; the next page, resumed
                // after the last of them, reaches it first.
                return match last {
                    Some(last) => Ok(Some(last)),
                    None => Err(error),
                };
            }
            last = Some(below.clone());
            out.push(Step {
                depth: step.depth,
                segment: Cow::Owned(step.segment.into_owned()),
                head: step.head.into_owned(),
            });
        }
        Ok(resume)
    }

    fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
        self.inner.changed_since(path, since)
    }
}

impl super::FileEngine {
    /// The test view reads go through, if a test asked for it.
    pub(super) fn copies(&self) -> Option<Arc<Switch>> {
        self.copies
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn switch(&self) -> Arc<Switch> {
        self.copies
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(Default::default)
            .clone()
    }

    /// Answer reads through the test view, checked against the file
    /// engine's (`Server::checking_reads`).
    pub(crate) fn check_reads(&self) {
        self.switch();
    }

    /// Fail every read of `path` (and below it) in the file named `file`,
    /// answering reads through the test view alone
    /// (`ServerHandle::failing_reads`).
    pub(crate) fn fail_reads(&self, file: &str, path: Vec<Vec<u8>>) {
        self.switch().fail(file, path);
    }
}

/// A server's switch to the test view (`Server::checking_reads`,
/// `ServerHandle::failing_reads`).
#[derive(Default)]
pub(crate) struct Switch {
    /// The paths of each file, by name, whose reads fail; when any is set,
    /// reads are answered through the test view alone.
    failing: Mutex<HashMap<String, Vec<Vec<Vec<u8>>>>>,
    /// Each file's derived state as the test view computed it, by the
    /// file engine's it was checked against (the same `Arc` while the
    /// file engine's has not changed).
    derived: Mutex<HashMap<u32, Computed>>,
}

/// The file engine's derived state of a file, and the test view's computed
/// when that was current.
type Computed = (Arc<Derived>, Arc<Derived>);

impl Switch {
    /// Fail every read of `path` in the file named `file` (and of what is
    /// below it).
    pub(crate) fn fail(&self, file: &str, path: Vec<Vec<u8>>) {
        self.failing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(file.to_lowercase())
            .or_default()
            .push(path);
    }

    fn failing(&self) -> Option<HashMap<String, Vec<Vec<Vec<u8>>>>> {
        let failing = self.failing.lock().unwrap_or_else(PoisonError::into_inner);
        (!failing.is_empty()).then(|| failing.clone())
    }

    /// Whether reads are answered twice and compared.
    pub(crate) fn checks(&self) -> bool {
        self.failing().is_none()
    }

    /// `read`, answered through the test view (module doc).
    pub(crate) fn serve<T: PartialEq + std::fmt::Debug>(
        &self,
        reading: &Reading<'_>,
        mut read: impl FnMut(Read<'_>) -> T,
    ) -> T {
        let failing = self.failing();
        let paths = failing
            .as_ref()
            .and_then(|failing| failing.get(&reading.file().name.to_lowercase()))
            .cloned()
            .unwrap_or_default();
        let file_read = reading.read();
        let copies = Copies::new(reading.file_view(), &paths);
        let derived = self.derived_of(reading, &copies, failing.is_none());
        let copied = Read {
            view: &copies,
            derived: &derived,
            ..file_read
        };
        if failing.is_some() {
            return read(copied);
        }
        let expected = read(file_read);
        let answer = read(copied);
        assert_eq!(
            answer,
            expected,
            "{:?}: the test view's answer differs from the file engine's",
            reading.file().name
        );
        answer
    }

    /// `read`, answered through the test view alone, unchecked: a read
    /// that may run only once.
    pub(crate) fn serve_once<T>(
        &self,
        reading: &Reading<'_>,
        read: impl FnOnce(Read<'_>) -> T,
    ) -> T {
        let failing = self.failing();
        let paths = failing
            .as_ref()
            .and_then(|failing| failing.get(&reading.file().name.to_lowercase()))
            .cloned()
            .unwrap_or_default();
        let copies = Copies::new(reading.file_view(), &paths);
        let derived = self.derived_of(reading, &copies, failing.is_none());
        read(Read {
            view: &copies,
            derived: &derived,
            ..reading.read()
        })
    }

    /// The derived state of the file `reading` reads, computed through
    /// `copies`: once for each load of the file, and the catalog again each
    /// time the file engine's changes, as the file engine reads it again
    /// after a schema change. With `check` it must equal the file engine's,
    /// and failing to compute it fails the check. Without (a test told the
    /// view to fail paths), a failure gives the file engine's state: derived
    /// state is computed when a file is loaded and when its schema changes,
    /// not by the read that fails, so a damaged part of the catalog fails
    /// the reads that reach it but leaves the state readers were given.
    fn derived_of(&self, reading: &Reading<'_>, copies: &Copies<'_>, check: bool) -> Arc<Derived> {
        let current = reading.derived();
        let id = reading.file().id;
        let mut cache = self.derived.lock().unwrap_or_else(PoisonError::into_inner);
        let computed = match cache.get(&id) {
            Some((of, mine)) if Arc::ptr_eq(of, current) => Ok(mine.clone()),
            // The same load of the file, its schema changed.
            Some((of, mine)) if of.instance() == current.instance() => {
                // The file engine computed its state again: the tree changed
                // since the version the last one was computed from.
                assert!(
                    copies.changed_since(&[], of.version()).unwrap_or(true),
                    "derived state changed without a change of the version"
                );
                catalog_of(reading, copies)
                    .map(|catalog| Arc::new(mine.with_catalog(copies.version(), catalog)))
            }
            // The first read of this load of the file (a file closed and
            // opened again is a new store, whose versions start over).
            _ => catalog_of(reading, copies).and_then(|catalog| {
                let mut mine = Derived::new(copies, catalog, current.name())?;
                if current.desktop() {
                    mine.serve_desktop();
                }
                // Not in the tree: what the engine loaded, as the name is.
                mine.set_page_zero(current.page_zero());
                Ok(Arc::new(mine))
            }),
        };
        match computed {
            Ok(mine) => {
                if check {
                    assert!(
                        mine.same_as(current),
                        "{:?}: the test view's derived state differs from the file engine's:\n{mine:?}\n{current:?}",
                        reading.file().name
                    );
                }
                cache.insert(id, (current.clone(), mine.clone()));
                mine
            }
            Err(error) if check => panic!(
                "{:?}: the test view could not compute derived state: {error}",
                reading.file().name
            ),
            Err(error) => {
                warn!(
                    "{:?}: the test view could not compute derived state ({error}); \
                     using the file engine's",
                    reading.file().name
                );
                current.clone()
            }
        }
    }
}

/// The catalog the test view's derived state holds: the one it decodes
/// through `copies`, or, when the file's catalog does not decode through
/// either view (the same error from both), the one the file engine kept.
/// The store keeps the catalog it last decoded when a schema change leaves
/// one it cannot decode (`Store::reread_catalog`); no view of this version
/// can give that one, which an earlier version held, so that is no
/// difference between the views. A catalog only the test view fails to
/// decode is its own failure.
fn catalog_of(reading: &Reading<'_>, copies: &Copies<'_>) -> om_hbam::Result<om_hbam::Catalog> {
    match derived::read_catalog(copies) {
        Ok(catalog) => Ok(catalog),
        Err(error) => match derived::read_catalog(reading.file_view()) {
            Err(theirs) if theirs.to_string() == error.to_string() => {
                Ok(reading.derived().catalog().clone())
            }
            _ => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::view::{wire, TreeView, NO_LIMIT};

    /// The test view reads what the tree view reads, copied and in pages of
    /// three, and fails what is at or below a failing path, and a subtree
    /// or walk that holds one.
    #[test]
    fn the_test_view_copies_pages_and_fails_as_told() {
        let mut root = OwnedNode::default();
        for id in 0..10u8 {
            root.put(&[&[1], &[id], &[0]], OwnedNode::leaf(Vec::new(), vec![id]));
        }
        root.put(&[&[2]], OwnedNode::leaf(Vec::new(), b"two".to_vec()));
        let tree = TreeView::new(&root, 7);
        let failing = vec![vec![vec![1], vec![5]]];
        let copies = Copies::new(&tree, &failing);
        let view: &dyn View = &copies;
        assert_eq!(view.version(), 7);
        // Untouched paths read the same, owned.
        assert!(matches!(
            view.subtree(&[&[2]], NO_LIMIT).unwrap(),
            Some(Cow::Owned(node)) if &node == root.at(&[&[2]]).unwrap()
        ));
        assert_eq!(wire(view, &[&[2]]).unwrap(), wire(&tree, &[&[2]]).unwrap());
        let mut page = Vec::new();
        assert!(view
            .children_page(
                &[&[1]],
                Bound::Unbounded,
                Direction::Forward,
                100,
                &mut page
            )
            .unwrap());
        assert_eq!(page.len(), PAGE);
        // Children before the failing one list; reaching it fails.
        let listed: Vec<Result<Child, ReadError>> = view
            .children(&[&[1]], Bound::Unbounded, Direction::Forward)
            .collect();
        assert_eq!(listed.len(), 6);
        assert!(listed[..5].iter().all(Result::is_ok));
        assert_eq!(listed[5], Err(unreadable(&failing[0])));
        // Backward from below it does not reach it.
        assert_eq!(
            view.children(&[&[1]], Bound::Excluded(&[5]), Direction::Backward)
                .count(),
            5
        );
        // Listing the children of it, or of a node below it, fails, also
        // when there are none to give: the node itself, a missing one, a
        // bound past its last child.
        let failing_list = vec![vec![vec![1], vec![4]], vec![vec![9]]];
        let listing = Copies::new(&tree, &failing_list);
        let listing: &dyn View = &listing;
        for (path, from) in [
            (&[&[1][..], &[4][..]][..], Bound::Unbounded),
            (&[&[1][..], &[4][..], &[0][..]][..], Bound::Unbounded),
            (&[&[9][..]][..], Bound::Unbounded),
            (&[&[1][..], &[4][..]][..], Bound::Excluded(&[7][..])),
        ] {
            let listed: Vec<_> = listing.children(path, from, Direction::Forward).collect();
            assert!(matches!(listed[..], [Err(_)]), "{path:02x?}");
            let whole: Vec<_> = listing
                .subtrees(path, from, Direction::Forward, NO_LIMIT)
                .collect();
            assert!(matches!(whole[..], [Err(_)]), "{path:02x?}");
        }
        assert!(wire(listing, &[&[9]]).is_err());
        // A page resumed after a node below the failing one fails, though
        // what follows that node is not below it.
        let mut out = Vec::new();
        let after = [vec![5], vec![0]];
        assert!(view.walk_page(&[&[1]], Some(&after), 10, &mut out).is_err());
        assert!(out.is_empty());
        // A read at or below it, a subtree or walk holding it, fail.
        assert!(view.node(&[&[1], &[5], &[0]]).is_err());
        assert!(view.node(&[&[1], &[4], &[0]]).is_ok());
        assert!(view.subtree(&[&[1]], NO_LIMIT).is_err());
        assert!(view.subtree(&[], NO_LIMIT).is_err());
        assert!(wire(view, &[&[1]]).is_err());
        assert!(wire(view, &[&[1], &[4]]).is_ok());
        // Without failing paths, every subtree's wire form is the tree's.
        let copies = Copies::new(&tree, &[]);
        for path in [&[][..], &[&[1][..]], &[&[1][..], &[3][..]]] {
            assert_eq!(wire(&copies, path), wire(&tree, path), "{path:02x?}");
        }
    }
}
