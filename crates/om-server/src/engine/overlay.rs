//! A view of one version with some of its subtrees covered
//! (docs/server-storage.md 5, "The overlay view"): each covered path is
//! read from the node laid over it, or as missing when nothing is, and
//! everything else from the version. A server script's find inside an
//! open transaction reads the transaction's records through one
//! (`scripts::host`), as it read a store that held them alone before.
//!
//! The rule, for a path:
//!
//! - **At or below a covered path**: answered from what lies over it
//!   alone, never from the version.
//! - **Above a covered path** (a node on the way to one): the version's
//!   node, with each covered child laid in its place, removed or added in
//!   key order; a node on the way that the version lacks is a list when
//!   something is laid below it. Its subtree is the version's with the
//!   covers applied.
//! - **Elsewhere**: the version's.
//!
//! A walk across covered and uncovered nodes gives them in the key order of
//! the tree the rule describes: the version's nodes up to a covered one,
//! the covered node's subtree as laid, then the version's nodes after it.
//! The overlay reads the version's version, and anything at, above or
//! below a cover counts as changed since any other.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Bound;

use om_hbam::OwnedNode;

use super::view::{page_of, within, Child, Direction, Head, ReadError, Step, Version, View};
use super::{TreeView, NO_LIMIT};

/// A covered path and what lies over it: a node, named by the path's last
/// segment, or nothing.
pub type Cover = (Vec<Vec<u8>>, Option<OwnedNode>);

/// A view of `base` with the paths of `covered` covered (the module doc).
/// No covered path is at or below another.
pub struct Overlay<'v> {
    base: &'v dyn View,
    covered: &'v [Cover],
}

/// Where a path is, as the rule sees it.
enum Place<'c> {
    /// At or below the covered path: the node there, if any.
    Covered(Option<&'c OwnedNode>),
    /// On the way to covered paths.
    Above,
    /// Untouched by any cover.
    Base,
}

fn starts_with(path: &[&[u8]], prefix: &[Vec<u8>]) -> bool {
    prefix.len() <= path.len() && prefix.iter().zip(path).all(|(a, b)| a.as_slice() == *b)
}

fn joined<'p>(path: &[&'p [u8]], rest: &'p [Vec<u8>]) -> Vec<&'p [u8]> {
    let mut joined = path.to_vec();
    joined.extend(rest.iter().map(Vec::as_slice));
    joined
}

impl<'v> Overlay<'v> {
    pub fn new(base: &'v dyn View, covered: &'v [Cover]) -> Self {
        debug_assert!(covered.iter().all(|(a, _)| {
            let a: Vec<&[u8]> = a.iter().map(Vec::as_slice).collect();
            covered.iter().filter(|(b, _)| starts_with(&a, b)).count() == 1
        }));
        Overlay { base, covered }
    }

    fn place(&self, path: &[&[u8]]) -> Place<'v> {
        for (covered, node) in self.covered {
            if starts_with(path, covered) {
                let below = &path[covered.len()..];
                return Place::Covered(node.as_ref().and_then(|node| node.at(below)));
            }
        }
        let above = self.covered.iter().any(|(covered, _)| {
            covered.len() > path.len() && starts_with(&path_of(covered), &owned(path))
        });
        if above {
            Place::Above
        } else {
            Place::Base
        }
    }

    /// The covers strictly below `path`, each with its path below `path`.
    fn below<'a>(
        &'a self,
        path: &'a [&'a [u8]],
    ) -> impl Iterator<Item = (&'v [Vec<u8>], Option<&'v OwnedNode>)> + 'a {
        self.covered.iter().filter_map(move |(covered, node)| {
            let covered_path = path_of(covered);
            (covered.len() > path.len() && starts_with(&covered_path, &owned(path)))
                .then(|| (&covered[path.len()..], node.as_ref()))
        })
    }

    /// Whether anything is laid below `path`.
    fn laid_below(&self, path: &[&[u8]]) -> bool {
        self.below(path).any(|(_, node)| node.is_some())
    }

    /// The children of `path`, a node above covers, that the covers decide:
    /// a covered child with what lies over it (nothing when it is removed),
    /// and a child on the way to a deeper cover as the rule reads it.
    fn decided(&self, path: &[&[u8]]) -> Result<BTreeMap<Vec<u8>, Option<Head<'_>>>, ReadError> {
        let mut decided = BTreeMap::new();
        let below: Vec<_> = self.below(path).collect();
        for (rest, node) in below {
            let segment = rest[0].clone();
            if rest.len() == 1 {
                decided.insert(segment, node.map(Head::of));
            } else if let std::collections::btree_map::Entry::Vacant(entry) = decided.entry(segment)
            {
                let mut child = path.to_vec();
                child.push(&rest[0]);
                entry.insert(self.node(&child)?);
            }
        }
        Ok(decided)
    }
}

fn path_of(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

fn owned(path: &[&[u8]]) -> Vec<Vec<u8>> {
    path.iter().map(|segment| segment.to_vec()).collect()
}

/// Whether `segment` comes after `from` in `direction`.
fn after(segment: &[u8], from: Bound<&[u8]>, direction: Direction) -> bool {
    match (direction, from) {
        (_, Bound::Unbounded) => true,
        (Direction::Forward, Bound::Included(from)) => segment >= from,
        (Direction::Forward, Bound::Excluded(from)) => segment > from,
        (Direction::Backward, Bound::Included(from)) => segment <= from,
        (Direction::Backward, Bound::Excluded(from)) => segment < from,
    }
}

/// Whether `a` comes before `b` in `direction`.
fn before(a: &[u8], b: &[u8], direction: Direction) -> bool {
    match direction {
        Direction::Forward => a < b,
        Direction::Backward => a > b,
    }
}

impl View for Overlay<'_> {
    fn version(&self) -> Version {
        self.base.version()
    }

    fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
        match self.place(path) {
            Place::Covered(node) => Ok(node.map(Head::of)),
            Place::Above => {
                let head = self.base.node(path)?;
                if !self.laid_below(path) {
                    return Ok(head);
                }
                Ok(Some(Head {
                    list: true,
                    value: head.and_then(|head| head.value),
                }))
            }
            Place::Base => self.base.node(path),
        }
    }

    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError> {
        match self.place(path) {
            Place::Covered(None) => Ok(None),
            Place::Covered(Some(node)) => {
                within(node, || owned(path), limit).map(|node| Some(Cow::Borrowed(node)))
            }
            Place::Above => {
                let base = self.base.subtree(path, NO_LIMIT)?.map(Cow::into_owned);
                let mut node = match base {
                    Some(node) => node,
                    None if self.laid_below(path) => OwnedNode::list(
                        path.last().map(|s| s.to_vec()).unwrap_or_default(),
                        Vec::new(),
                    ),
                    None => return Ok(None),
                };
                for (rest, laid) in self.below(path) {
                    let rest = path_of(rest);
                    match laid {
                        Some(laid) => node.put(&rest, laid.clone()),
                        None => {
                            node.remove(&rest);
                        }
                    }
                }
                within(&node, || owned(path), limit)?;
                Ok(Some(Cow::Owned(node)))
            }
            Place::Base => self.base.subtree(path, limit),
        }
    }

    fn children_page<'a>(
        &'a self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'a>>,
    ) -> Result<bool, ReadError> {
        match self.place(path) {
            Place::Covered(None) => Ok(false),
            Place::Covered(Some(node)) => page_of(node, from, direction, max, |child| {
                out.push(Child {
                    segment: Cow::Borrowed(&child.segment),
                    head: Head::of(child),
                });
                Ok(())
            }),
            Place::Base => self.base.children_page(path, from, direction, max, out),
            Place::Above => {
                let decided = self.decided(path)?;
                let mut from: Bound<Vec<u8>> = match from {
                    Bound::Included(s) => Bound::Included(s.to_vec()),
                    Bound::Excluded(s) => Bound::Excluded(s.to_vec()),
                    Bound::Unbounded => Bound::Unbounded,
                };
                loop {
                    let mut page = Vec::new();
                    let more = self.base.children_page(
                        path,
                        from.as_ref().map(Vec::as_slice),
                        direction,
                        max,
                        &mut page,
                    )?;
                    let last = page.last().map(|child| child.segment.to_vec());
                    // The version's children the covers do not decide, and
                    // the decided ones in this page's range: up to its last
                    // child when more may follow it.
                    let mut merged: Vec<Child<'a>> = page
                        .into_iter()
                        .filter(|child| !decided.contains_key(&*child.segment))
                        .collect();
                    for (segment, head) in &decided {
                        let Some(head) = head else {
                            continue;
                        };
                        let in_range = after(segment, from.as_ref().map(Vec::as_slice), direction)
                            && match (&last, more) {
                                (Some(last), true) => !before(last, segment, direction),
                                _ => true,
                            };
                        if in_range {
                            merged.push(Child {
                                segment: Cow::Owned(segment.clone()),
                                head: head.clone().into_owned(),
                            });
                        }
                    }
                    merged.sort_by(|a, b| match direction {
                        Direction::Forward => a.segment.cmp(&b.segment),
                        Direction::Backward => b.segment.cmp(&a.segment),
                    });
                    let cut = merged.len() > max;
                    let given = !merged.is_empty();
                    out.extend(merged.into_iter().take(max));
                    match (given, more, last) {
                        (true, _, _) | (false, false, _) | (false, true, None) => {
                            return Ok(cut || more)
                        }
                        // Every child of this page was covered away: the
                        // next page, after it.
                        (false, true, Some(last)) => from = Bound::Excluded(last),
                    }
                }
            }
        }
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
        match self.place(path) {
            Place::Covered(None) => Ok(false),
            Place::Covered(Some(node)) => page_of(node, from, direction, max, |child| {
                let path = || {
                    let mut path = owned(path);
                    path.push(child.segment.clone());
                    path
                };
                out.push(Cow::Borrowed(within(child, path, limit)?));
                Ok(())
            }),
            Place::Base => self
                .base
                .subtrees_page(path, from, direction, max, limit, out),
            Place::Above => {
                let mut children = Vec::new();
                let more = self.children_page(path, from, direction, max, &mut children)?;
                for child in children {
                    let mut at = path.to_vec();
                    at.push(&child.segment);
                    if let Some(node) = self.subtree(&at, limit)? {
                        out.push(Cow::Owned(node.into_owned()));
                    }
                }
                Ok(more)
            }
        }
    }

    fn walk_page<'a>(
        &'a self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'a>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        match self.place(path) {
            Place::Covered(None) => Ok(None),
            Place::Covered(Some(node)) => {
                Ok(TreeView::new(node, self.version()).walk_into(&[], after, max, out))
            }
            Place::Base => self.base.walk_page(path, after, max, out),
            Place::Above => self.walk_above(path, after, max, out),
        }
    }

    fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
        match self.place(path) {
            Place::Base => self.base.changed_since(path, since),
            Place::Covered(_) | Place::Above => Ok(true),
        }
    }
}

impl Overlay<'_> {
    /// A depth-first walk of the subtree at `path`, a node above covers,
    /// from the overlay's own nodes and children (the rule's tree), each
    /// step owned.
    fn walk_above<'a>(
        &'a self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'a>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        let mut given = 0;
        let mut at: Vec<Vec<u8>> = match after {
            Some(after) => after.to_vec(),
            None => {
                let Some(head) = self.node(path)? else {
                    return Ok(None);
                };
                out.push(Step {
                    depth: 0,
                    segment: Cow::Owned(path.last().map(|s| s.to_vec()).unwrap_or_default()),
                    head: head.into_owned(),
                });
                given += 1;
                Vec::new()
            }
        };
        while given < max.max(1) {
            // The first child of the node reached, else the next sibling of
            // it or of the nearest node above it that has one.
            let mut first = Vec::new();
            self.children_page(
                &joined(path, &at),
                Bound::Unbounded,
                Direction::Forward,
                1,
                &mut first,
            )?;
            let next = match first.pop() {
                Some(child) => Some(child),
                None => loop {
                    let Some(last) = at.pop() else {
                        return Ok(None);
                    };
                    let mut sibling = Vec::new();
                    self.children_page(
                        &joined(path, &at),
                        Bound::Excluded(&last),
                        Direction::Forward,
                        1,
                        &mut sibling,
                    )?;
                    if let Some(sibling) = sibling.pop() {
                        break Some(sibling);
                    }
                },
            };
            let Some(child) = next else {
                return Ok(None);
            };
            at.push(child.segment.to_vec());
            out.push(Step {
                depth: at.len(),
                segment: Cow::Owned(child.segment.into_owned()),
                head: child.head.into_owned(),
            });
            given += 1;
        }
        Ok(Some(at))
    }
}
