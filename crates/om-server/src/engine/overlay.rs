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
//!   key order. The tree is the version's with each cover applied as
//!   `OwnedNode::put` and `OwnedNode::remove` apply it, and every read
//!   gives that tree's answer: a node with something laid below it is a
//!   list, one the version lacks with no value, and a leaf that held an
//!   empty value loses it (`OwnedNode::become_list`); a node with only
//!   removals below it is the version's.
//! - **Elsewhere**: the version's.
//!
//! What a cover hides is never read: the children of a node above covers
//! are the version's listing with each covered child substituted, and a
//! covered child the version cannot read does not fail the listing. A
//! subtree above covers is built from that listing, a child at a time, and
//! refused once what it holds passes the limit, before the rest is read.
//!
//! A walk across covered and uncovered nodes gives them in the key order of
//! the tree the rule describes: the version's nodes up to a covered one,
//! the covered node's subtree as laid, then the version's nodes after it.
//! The overlay reads the version's version, and anything at, above or
//! below a cover counts as changed since any other.

use std::borrow::Cow;
use std::collections::{BTreeMap, VecDeque};
use std::ops::Bound;

use om_hbam::OwnedNode;

use super::view::{page_of, within, Child, Direction, Head, ReadError, Step, Version, View};
use super::{TreeView, NO_LIMIT};

/// The children a subtree above covers is built from at a time.
const PAGE: usize = 256;

/// A covered path and what lies over it: a node, named by the path's last
/// segment, or nothing. A cover with the empty path changes nothing, as
/// `OwnedNode::put` and `OwnedNode::remove` change nothing at it.
pub type Cover = (Vec<Vec<u8>>, Option<OwnedNode>);

/// A view of `base` with the paths of `covered` covered (the module doc).
/// No covered path is at or below another.
pub struct Overlay<'v> {
    base: &'v dyn View,
    /// The covers, without any of the empty path ([`Cover`]).
    covered: Vec<(&'v [Vec<u8>], Option<&'v OwnedNode>)>,
}

/// What the covers decide of a child of a node above them, from the covers
/// alone (no read).
#[derive(Clone, Copy)]
enum Decided<'c> {
    /// A covered child: what lies over it, or nothing.
    Laid(Option<&'c OwnedNode>),
    /// A child on the way to a deeper cover, whose head the rule reads
    /// ([`View::node`] of the overlay).
    Way,
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
        let covered: Vec<_> = covered
            .iter()
            .filter(|(path, _)| !path.is_empty())
            .map(|(path, node)| (path.as_slice(), node.as_ref()))
            .collect();
        debug_assert!(covered.iter().all(|&(a, _)| {
            let a: Vec<&[u8]> = a.iter().map(Vec::as_slice).collect();
            covered.iter().filter(|&&(b, _)| starts_with(&a, b)).count() == 1
        }));
        Overlay { base, covered }
    }

    fn place(&self, path: &[&[u8]]) -> Place<'v> {
        for &(covered, node) in &self.covered {
            if starts_with(path, covered) {
                let below = &path[covered.len()..];
                return Place::Covered(node.and_then(|node| node.at(below)));
            }
        }
        let above = self.covered.iter().any(|&(covered, _)| {
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
        self.covered.iter().filter_map(move |&(covered, node)| {
            let covered_path = path_of(covered);
            (covered.len() > path.len() && starts_with(&covered_path, &owned(path)))
                .then(|| (&covered[path.len()..], node))
        })
    }

    /// Whether anything is laid below `path`.
    fn laid_below(&self, path: &[&[u8]]) -> bool {
        self.below(path).any(|(_, node)| node.is_some())
    }

    /// The children of `path`, a node above covers, that the covers
    /// decide, by segment, from the covers alone: nothing is read.
    fn decided(&self, path: &[&[u8]]) -> BTreeMap<&'v [u8], Decided<'v>> {
        let mut decided = BTreeMap::new();
        let below: Vec<_> = self.below(path).collect();
        for (rest, node) in below {
            let segment = rest[0].as_slice();
            if rest.len() == 1 {
                decided.insert(segment, Decided::Laid(node));
            } else {
                decided.entry(segment).or_insert(Decided::Way);
            }
        }
        decided
    }

    /// The child of `path` a failed read of the version reached, and
    /// whether a cover hides it (the failure is at or below a cover, which
    /// the overlay does not read); `None` when the failure is not below
    /// `path`.
    fn failed_child(&self, path: &[&[u8]], error: &ReadError) -> Option<(Vec<u8>, bool)> {
        let ReadError::Unreadable { path: failed, .. } = error else {
            return None;
        };
        let failed_path = path_of(failed);
        if failed.len() <= path.len() || !starts_with(&failed_path, &owned(path)) {
            return None;
        }
        let hidden = self
            .covered
            .iter()
            .any(|&(covered, _)| starts_with(&failed_path, covered));
        Some((failed[path.len()].clone(), hidden))
    }

    /// [`View::children_page`] of `path`, a node above covers: the
    /// version's children with those the covers decide in their place,
    /// merged in `direction` from `from`. The bound is applied before
    /// anything is read or decided; the version is asked only for as many
    /// children as the page can still use; a decided child's head is lent
    /// from what lies over it, or read for a child on the way, only when
    /// the page gives it. A child of the version that cannot be read fails
    /// the page when the page reaches it, and not at all when a cover hides
    /// it or the page ends before it. A view gives the children before one
    /// it cannot read and fails when that one comes first (`super::copies`),
    /// so nothing unlisted lies before it.
    fn page_above<'a>(
        &'a self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'a>>,
    ) -> Result<bool, ReadError> {
        let decided = self.decided(path);
        let mut ahead: Vec<(&'v [u8], Decided<'v>)> = decided
            .iter()
            .filter(|(segment, _)| after(segment, from, direction))
            .map(|(segment, decided)| (*segment, *decided))
            .collect();
        if direction == Direction::Backward {
            ahead.reverse();
        }
        let mut ahead = ahead.into_iter().peekable();
        let mut version: VecDeque<Child<'a>> = VecDeque::new();
        let mut version_from: Bound<Vec<u8>> = from.map(<[u8]>::to_vec);
        let mut version_more = true;
        // A child of the version that could not be read and that no cover
        // hides: the page fails if it reaches it.
        let mut unreadable: Option<(Vec<u8>, ReadError)> = None;
        let mut given = 0;
        loop {
            if given >= max {
                let left = !version.is_empty()
                    || version_more
                    || unreadable.is_some()
                    || ahead.peek().is_some();
                return Ok(left);
            }
            while version.is_empty() && version_more && unreadable.is_none() {
                let mut page = Vec::new();
                let listed = self.base.children_page(
                    path,
                    version_from.as_ref().map(Vec::as_slice),
                    direction,
                    max - given,
                    &mut page,
                );
                let got = !page.is_empty();
                if let Some(last) = page.last() {
                    version_from = Bound::Excluded(last.segment.to_vec());
                }
                version.extend(
                    page.into_iter()
                        .filter(|child| !decided.contains_key(&*child.segment)),
                );
                match listed {
                    Ok(more) => version_more = more && got,
                    Err(error) => match self.failed_child(path, &error) {
                        // A child a cover hides: the listing goes on after it.
                        Some((segment, true)) => version_from = Bound::Excluded(segment),
                        Some((segment, false)) => unreadable = Some((segment, error)),
                        None => return Err(error),
                    },
                }
            }
            let take_decided = {
                let next_version = version
                    .front()
                    .map(|child| &*child.segment)
                    .or(unreadable.as_ref().map(|(segment, _)| segment.as_slice()));
                match (ahead.peek(), next_version) {
                    (None, None) => return Ok(false),
                    (Some(_), None) => true,
                    (None, Some(_)) => false,
                    (Some((decided, _)), Some(next)) => before(decided, next, direction),
                }
            };
            if take_decided {
                let (segment, decision) = ahead.next().expect("peeked");
                let head = match decision {
                    Decided::Laid(laid) => laid.map(Head::of),
                    Decided::Way => {
                        let mut child = path.to_vec();
                        child.push(segment);
                        self.node(&child)?
                    }
                };
                if let Some(head) = head {
                    out.push(Child {
                        segment: Cow::Borrowed(segment),
                        head,
                    });
                    given += 1;
                }
            } else if let Some(child) = version.pop_front() {
                out.push(child);
                given += 1;
            } else {
                let (_, error) = unreadable.take().expect("the version's next child");
                return Err(error);
            }
        }
    }

    /// The subtree at `path`, a node above covers, built from its children
    /// as the overlay lists them: each covered child from what lies over
    /// it, each child above covers built so in turn, each other child from
    /// the version, so that nothing a cover hides is read. What it holds
    /// is counted as it is built against `limit`, `room` being what is
    /// left of it, and the subtree at `top` refused once it passes.
    fn build_above(
        &self,
        path: &[&[u8]],
        top: &[&[u8]],
        limit: usize,
        room: &mut usize,
    ) -> Result<Option<OwnedNode>, ReadError> {
        let too_large = || ReadError::TooLarge {
            path: owned(top),
            limit,
        };
        let charge = |room: &mut usize, cost: usize| {
            if limit == NO_LIMIT {
                return Ok(());
            }
            *room = room.checked_sub(cost).ok_or_else(too_large)?;
            Ok(())
        };
        let Some(head) = self.node(path)? else {
            return Ok(None);
        };
        let segment = path.last().map(|s| s.to_vec()).unwrap_or_default();
        // Charged before its value is copied.
        let value = head.value.as_ref().map_or(0, |value| value.len());
        charge(room, size_of::<OwnedNode>() + segment.len() + value)?;
        let mut node = OwnedNode {
            segment,
            value: head.value.map(Cow::into_owned),
            push: head.list,
            children: Vec::new(),
        };
        let mut from: Option<Vec<u8>> = None;
        loop {
            // No more children than could still fit, each at least a
            // node's place, and one more to pass the limit with.
            let page_size = match limit {
                NO_LIMIT => PAGE,
                _ => PAGE.min(*room / size_of::<OwnedNode>() + 1),
            };
            let mut page = Vec::new();
            let bound = match &from {
                Some(segment) => Bound::Excluded(segment.as_slice()),
                None => Bound::Unbounded,
            };
            let more = self.children_page(path, bound, Direction::Forward, page_size, &mut page)?;
            let Some(last) = page.last().map(|child| child.segment.to_vec()) else {
                break;
            };
            for child in page {
                let mut at = path.to_vec();
                at.push(&child.segment);
                let built = match self.place(&at) {
                    Place::Covered(None) => None,
                    Place::Covered(Some(laid)) => {
                        if limit != NO_LIMIT {
                            let held =
                                om_hbam::tree::memory_within(laid, *room).ok_or_else(too_large)?;
                            charge(room, held)?;
                        }
                        Some(laid.clone())
                    }
                    Place::Above => self.build_above(&at, top, limit, room)?,
                    Place::Base => {
                        let read = match self.base.subtree(&at, *room) {
                            Err(ReadError::TooLarge { .. }) => return Err(too_large()),
                            read => read?,
                        };
                        let built = read.map(Cow::into_owned);
                        if let Some(built) = &built {
                            charge(room, om_hbam::tree::memory(built))?;
                        }
                        built
                    }
                };
                node.children.extend(built);
            }
            if !more {
                break;
            }
            from = Some(last);
        }
        node.children.shrink_to_fit();
        Ok(Some(node))
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
                // A list, as `OwnedNode::put` makes it: the empty value of
                // a leaf goes, any other value stays.
                let value = head.and_then(|head| match head.value {
                    Some(value) if !head.list && value.is_empty() => None,
                    value => value,
                });
                Ok(Some(Head { list: true, value }))
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
                let mut room = limit;
                let Some(node) = self.build_above(path, path, limit, &mut room)? else {
                    return Ok(None);
                };
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
            Place::Above => self.page_above(path, from, direction, max, out),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::copies::Copies;
    use crate::engine::view::wire;

    fn leaf(value: &[u8]) -> OwnedNode {
        OwnedNode::leaf(Vec::new(), value.to_vec())
    }

    /// Every path of `node`'s tree below `at`, `at` first.
    fn paths(node: &OwnedNode, at: Vec<Vec<u8>>, out: &mut Vec<Vec<Vec<u8>>>) {
        out.push(at.clone());
        for child in &node.children {
            let mut path = at.clone();
            path.push(child.segment.clone());
            paths(child, path, out);
        }
    }

    /// The children of `path` from `from` in `direction`, read `max` at a
    /// time.
    fn listed(
        view: &dyn View,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
    ) -> Result<Vec<Child<'static>>, ReadError> {
        let mut all: Vec<Child<'static>> = Vec::new();
        let mut from = from.map(<[u8]>::to_vec);
        loop {
            let mut page = Vec::new();
            let more = view.children_page(
                path,
                from.as_ref().map(Vec::as_slice),
                direction,
                max,
                &mut page,
            )?;
            all.extend(page.into_iter().map(|child| Child {
                segment: Cow::Owned(child.segment.into_owned()),
                head: child.head.into_owned(),
            }));
            match (more, all.last()) {
                (true, Some(last)) => from = Bound::Excluded(last.segment.to_vec()),
                _ => return Ok(all),
            }
        }
    }

    /// Whether `a` answers every read of `path` as `b` does.
    fn same_answers(a: &dyn View, b: &dyn View, path: &[&[u8]]) {
        let at = format!("{path:02x?}");
        assert_eq!(a.node(path), b.node(path), "node {at}");
        let subtree = |view: &dyn View| {
            view.subtree(path, NO_LIMIT)
                .map(|node| node.map(Cow::into_owned))
        };
        assert_eq!(subtree(a), subtree(b), "subtree {at}");
        assert_eq!(wire(a, path), wire(b, path), "walk {at}");
        let children = listed(b, path, Bound::Unbounded, Direction::Forward, NO_LIMIT).unwrap();
        let mut bounds = vec![Bound::Unbounded];
        for child in &children {
            bounds.push(Bound::Included(child.segment.to_vec()));
            bounds.push(Bound::Excluded(child.segment.to_vec()));
        }
        for from in &bounds {
            let from = from.as_ref().map(Vec::as_slice);
            for direction in [Direction::Forward, Direction::Backward] {
                for max in [1, 2, NO_LIMIT] {
                    assert_eq!(
                        listed(a, path, from, direction, max),
                        listed(b, path, from, direction, max),
                        "children {at} from {from:02x?} {direction:?} by {max}"
                    );
                }
                let subtrees = |view: &dyn View| -> Vec<_> {
                    view.subtrees(path, from, direction, NO_LIMIT)
                        .map(|node| node.map(Cow::into_owned))
                        .collect()
                };
                assert_eq!(subtrees(a), subtrees(b), "subtrees {at} from {from:02x?}");
            }
        }
    }

    /// The overlay answers every read as a tree view of the version with
    /// the covers applied by `OwnedNode::put` and `OwnedNode::remove`, the
    /// reference: a record replaced, removed and added; a cover two deep
    /// below a leaf that held an empty value, which becomes a list without
    /// it; below a leaf with a value, which keeps it; only removals below a
    /// list; covers below nodes the version lacks. Through a view that
    /// fails reads of what the covers hide, it answers the same; and a
    /// subtree above covers is refused at its limit before it reads past
    /// it.
    #[test]
    fn the_overlay_reads_as_the_version_with_its_covers_applied() {
        let mut base = OwnedNode::default();
        for id in 1..=6u8 {
            base.put(&[&[1], &[id], &[0]], leaf(&[id]));
            base.put(&[&[1], &[id], &[1]], leaf(b"text"));
        }
        base.put(&[&[2]], leaf(b""));
        base.put(&[&[3]], leaf(b"three"));
        for id in 1..=3u8 {
            base.put(&[&[4], &[id]], leaf(&[id]));
        }
        base.put(&[&[5]], leaf(b"five"));
        let mut changed = OwnedNode::list(Vec::new(), vec![leaf(b"changed")]);
        changed.children[0].segment = vec![0];
        // Each laid node named by its path's last segment (`Cover`).
        let laid = |path: Vec<Vec<u8>>, node: Option<OwnedNode>| -> Cover {
            let node = node.map(|mut node| {
                node.segment = path.last().unwrap().clone();
                node
            });
            (path, node)
        };
        let covers: Vec<Cover> = vec![
            laid(vec![vec![1], vec![2]], Some(changed.clone())),
            laid(vec![vec![1], vec![4]], None),
            laid(vec![vec![1], vec![9]], Some(changed.clone())),
            laid(vec![vec![2], vec![1], vec![1]], Some(leaf(b"below"))),
            laid(vec![vec![3], vec![1]], Some(leaf(b"below"))),
            laid(vec![vec![4], vec![2]], None),
            laid(vec![vec![6], vec![1]], Some(leaf(b"added"))),
            laid(vec![vec![7], vec![1]], None),
        ];
        let mut reference = base.clone();
        for (path, laid) in &covers {
            let path = path_of(path);
            match laid {
                Some(laid) => reference.put(&path, laid.clone()),
                None => {
                    reference.remove(&path);
                }
            }
        }
        // The rule's two heads for a node above a cover.
        assert_eq!(reference.at(&[&[2]]).unwrap().value, None);
        assert_eq!(
            reference.at(&[&[3]]).unwrap().value.as_deref(),
            Some(&b"three"[..])
        );
        let version = TreeView::new(&base, 3);
        let expected = TreeView::new(&reference, 3);
        let mut all = Vec::new();
        paths(&reference, Vec::new(), &mut all);
        paths(&base, Vec::new(), &mut all);
        all.extend([vec![vec![7]], vec![vec![7], vec![1]], vec![vec![8]]]);
        all.sort();
        all.dedup();
        let hiding: Vec<Vec<Vec<u8>>> = vec![
            vec![vec![1], vec![2]],
            vec![vec![1], vec![4]],
            vec![vec![4], vec![2]],
        ];
        let failing = Copies::new(&version, &hiding);
        for base in [&version as &dyn View, &failing] {
            let overlay = Overlay::new(base, &covers);
            for path in &all {
                same_answers(&overlay, &expected, &path_of(path));
            }
        }
        // A subtree above covers is refused at its limit, before it reads
        // a record that cannot be read further on.
        let later: Vec<Vec<Vec<u8>>> = vec![vec![vec![1], vec![6]]];
        let failing = Copies::new(&version, &later);
        let overlay = Overlay::new(&failing, &covers);
        assert!(matches!(
            overlay.subtree(&[&[1]], 200),
            Err(ReadError::TooLarge { limit: 200, .. })
        ));
        assert!(matches!(
            overlay.subtree(&[&[1]], NO_LIMIT),
            Err(ReadError::Unreadable { .. })
        ));
    }

    /// The request an overlay's page made of its version may cover:
    /// children of `path` from `from` in `direction`, at most `max`.
    struct Asked {
        path: Vec<Vec<u8>>,
        from: Bound<Vec<u8>>,
        direction: Direction,
        max: usize,
    }

    /// A view over `inner` that refuses, with an error, what a page of the
    /// overlay above covers must not read of its version: children of the
    /// asked node from before the page's bound or more of them than the
    /// page's size, or the head of a child before the bound. It counts the
    /// children it gives of that node and the subtrees read.
    struct Strict<'v> {
        inner: &'v dyn View,
        asked: std::cell::RefCell<Option<Asked>>,
        given: std::cell::Cell<usize>,
        subtrees: std::cell::Cell<usize>,
    }

    impl<'v> Strict<'v> {
        fn new(inner: &'v dyn View) -> Self {
            Strict {
                inner,
                asked: Default::default(),
                given: Default::default(),
                subtrees: Default::default(),
            }
        }

        fn ask(&self, path: &[&[u8]], from: Bound<&[u8]>, direction: Direction, max: usize) {
            *self.asked.borrow_mut() = Some(Asked {
                path: owned(path),
                from: from.map(<[u8]>::to_vec),
                direction,
                max,
            });
            self.given.set(0);
            self.subtrees.set(0);
        }
    }

    fn refused(why: &str) -> ReadError {
        ReadError::Unreadable {
            path: Vec::new(),
            why: why.into(),
        }
    }

    /// Whether every segment `inner` allows in `direction` is one `outer`
    /// allows.
    fn within_bound(inner: &Bound<&[u8]>, outer: &Bound<Vec<u8>>, direction: Direction) -> bool {
        let start = |bound: &Bound<&[u8]>| match bound {
            Bound::Included(s) => Some((s.to_vec(), true)),
            Bound::Excluded(s) => Some((s.to_vec(), false)),
            Bound::Unbounded => None,
        };
        let outer = start(&outer.as_ref().map(Vec::as_slice));
        match (start(inner), outer) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some((i, i_incl)), Some((o, o_incl))) => {
                before(&o, &i, direction) || (i == o && (o_incl || !i_incl))
            }
        }
    }

    impl View for Strict<'_> {
        fn version(&self) -> Version {
            self.inner.version()
        }

        fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
            if let Some(asked) = self.asked.borrow().as_ref() {
                let child = path.len() == asked.path.len() + 1 && starts_with(path, &asked.path);
                if child
                    && !after(
                        path[asked.path.len()],
                        asked.from.as_ref().map(Vec::as_slice),
                        asked.direction,
                    )
                {
                    return Err(refused("the head of a child before the page's bound"));
                }
            }
            self.inner.node(path)
        }

        fn subtree(
            &self,
            path: &[&[u8]],
            limit: usize,
        ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError> {
            self.subtrees.set(self.subtrees.get() + 1);
            self.inner.subtree(path, limit)
        }

        fn children_page<'a>(
            &'a self,
            path: &[&[u8]],
            from: Bound<&[u8]>,
            direction: Direction,
            max: usize,
            out: &mut Vec<Child<'a>>,
        ) -> Result<bool, ReadError> {
            let mut this = false;
            if let Some(asked) = self.asked.borrow().as_ref() {
                if path == path_of(&asked.path).as_slice() {
                    if direction != asked.direction || !within_bound(&from, &asked.from, direction)
                    {
                        return Err(refused("children from before the page's bound"));
                    }
                    if max > asked.max {
                        return Err(refused("more children than the page can give"));
                    }
                    this = true;
                }
            }
            let before = out.len();
            let more = self.inner.children_page(path, from, direction, max, out)?;
            if this {
                self.given.set(self.given.get() + out.len() - before);
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
            self.inner
                .subtrees_page(path, from, direction, max, limit, out)
        }

        fn walk_page<'a>(
            &'a self,
            path: &[&[u8]],
            after: Option<&[Vec<u8>]>,
            max: usize,
            out: &mut Vec<Step<'a>>,
        ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
            self.inner.walk_page(path, after, max, out)
        }

        fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
            self.inner.changed_since(path, since)
        }
    }

    /// A page of an overlay node above covers costs what it gives
    /// (docs/server-storage.md 5, "The overlay view"). Over a version
    /// whose node lists sixteen laid records before its own, with large
    /// values, a replaced, a removed and an unreadable record of its own,
    /// one on the way to a deeper cover that cannot be read, and one laid
    /// after them, every page from every bound in both directions, by 1, 2
    /// and 3: it gives what the reference tree gives, and fails exactly
    /// when it reaches a record that cannot be read and that no cover
    /// hides; it asks the version for no child before its bound, for no
    /// more than it can give, and is given no more than that and the
    /// decided children it skips; a laid head is lent, not copied. A
    /// subtree with a limit smaller than the first laid record is refused
    /// before any record of the version is read. A cover with the empty
    /// path changes nothing.
    #[test]
    fn an_overlay_page_costs_what_it_gives() {
        let mut base = OwnedNode::default();
        for id in 0x20..=0x2fu8 {
            base.put(&[&[1], &[id]], leaf(&[id]));
        }
        base.put(&[&[1], &[0x25], &[1]], leaf(b"deep"));
        let big = vec![7u8; 64 * 1024];
        let mut covers: Vec<Cover> = (0x01..=0x10u8)
            .map(|id| {
                (
                    vec![vec![1], vec![id]],
                    Some(OwnedNode::leaf(vec![id], big.clone())),
                )
            })
            .collect();
        covers.push((
            vec![vec![1], vec![0x22]],
            Some(OwnedNode::leaf(vec![0x22], b"changed".to_vec())),
        ));
        covers.push((vec![vec![1], vec![0x23]], None));
        covers.push((
            vec![vec![1], vec![0x25], vec![2]],
            Some(OwnedNode::leaf(vec![2], b"added".to_vec())),
        ));
        covers.push((
            vec![vec![1], vec![0x40]],
            Some(OwnedNode::leaf(vec![0x40], b"last".to_vec())),
        ));
        covers.push((Vec::new(), None));
        let mut reference = base.clone();
        for (path, laid) in &covers {
            let path = path_of(path);
            match laid {
                Some(laid) => reference.put(&path, laid.clone()),
                None => {
                    reference.remove(&path);
                }
            }
        }
        let version = TreeView::new(&base, 3);
        let expected = TreeView::new(&reference, 3);
        // Hidden: 0x22 replaced, 0x23 removed. Not hidden: 0x20 and 0x2a
        // of the version's own, and 0x25, on the way to a deeper cover.
        let unreadable: [u8; 3] = [0x20, 0x25, 0x2a];
        let failing: Vec<Vec<Vec<u8>>> = [0x20u8, 0x22, 0x23, 0x25, 0x2a]
            .iter()
            .map(|id| vec![vec![1], vec![*id]])
            .collect();
        let copies = Copies::new(&version, &failing);
        let strict = Strict::new(&copies);
        let overlay = Overlay::new(&strict, &covers);
        let laid: Vec<u8> = (0x01..=0x10).chain([0x22, 0x40]).collect();
        let segments: Vec<Vec<u8>> = reference
            .at(&[&[1]])
            .unwrap()
            .children
            .iter()
            .map(|c| c.segment.clone())
            .collect();
        let mut bounds = vec![Bound::Unbounded];
        for segment in segments
            .iter()
            .chain([&vec![0x23], &vec![0x00], &vec![0x50]])
        {
            bounds.push(Bound::Included(segment.clone()));
            bounds.push(Bound::Excluded(segment.clone()));
        }
        for from in &bounds {
            let from = from.as_ref().map(Vec::as_slice);
            for direction in [Direction::Forward, Direction::Backward] {
                for max in [1, 2, 3] {
                    let at = format!("from {from:02x?} {direction:?} by {max}");
                    let mut want = Vec::new();
                    expected
                        .children_page(&[&[1]], from, direction, max, &mut want)
                        .unwrap();
                    strict.ask(&[&[1]], from, direction, max);
                    let mut got = Vec::new();
                    let page = overlay.children_page(&[&[1]], from, direction, max, &mut got);
                    let reaches = want.iter().any(|c| unreadable.contains(&c.segment[0]));
                    match page {
                        Err(error) => assert!(reaches, "{at}: {error}"),
                        Ok(_) => {
                            assert!(!reaches, "{at}: reaches an unreadable record");
                            assert_eq!(got, want, "{at}");
                        }
                    }
                    assert!(
                        strict.given.get() <= max + 3,
                        "{at}: {} given",
                        strict.given.get()
                    );
                    for child in &got {
                        if laid.contains(&child.segment[0]) {
                            assert!(
                                matches!(child.head.value, Some(Cow::Borrowed(_))),
                                "{at}: a laid head copied"
                            );
                        }
                    }
                }
            }
        }
        // A limit smaller than the first laid record: refused before any
        // record of the version is read.
        let limit = 1024;
        strict.ask(
            &[&[1]],
            Bound::Unbounded,
            Direction::Forward,
            limit / size_of::<OwnedNode>() + 1,
        );
        assert!(matches!(
            overlay.subtree(&[&[1]], limit),
            Err(ReadError::TooLarge { .. })
        ));
        assert_eq!(strict.subtrees.get(), 0, "no record of the version read");
    }
}
