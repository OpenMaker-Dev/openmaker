//! The reads the upkeep of a stored index needs (docs/server-storage.md
//! 5, "Moving the other readers", item 5), over the listing a find reads an
//! index through ([`Listing`]), so that a host whose writes go through a
//! transaction of pages can keep the indexes without holding one whole:
//!
//! - a commit's moves of a record read and change only the entries under
//!   the keys its old and new values give ([`entries_at`]), and whether the
//!   index holds more entries than the record leaves, which a move that may
//!   empty the index needs ([`holds_more_than`]);
//! - the check that an index holds the keys the current rules give every
//!   record compares the index with one built from the records, in one pass
//!   that stops at the first difference ([`holds_exactly`]);
//! - a sweep reads every entry once, in key order ([`entries`]).
//!
//! Which fields of a table have which indexes is read from the heads of
//! `[T, 01, F]`'s children, not from a listing of entries, and the index
//! node's own value from its head.
//!
//! A listing is read a page at a time, each page from just past the last
//! key it gave, its pages growing to [`PAGE`]. A key out of the listing's
//! order (or outside where its page was asked from) is an error
//! ([`om_hbam::Error::KeyOrder`]), so a broken source can neither repeat an
//! entry nor make a read loop.

use std::borrow::Cow;
use std::collections::{BTreeMap, VecDeque};
use std::ops::Bound;

use om_hbam::{OwnedNode, RidList};

use crate::source::{Direction, Listing};

/// The most entries a page asks for, after a first one of [`FIRST_PAGE`].
pub const PAGE: usize = 256;
const FIRST_PAGE: usize = 8;

fn out_of_order() -> om_hbam::Error {
    om_hbam::Error::KeyOrder {
        path: "an index's listing".into(),
    }
}

/// Every entry of `listing` from `from` on in `direction`, each with what
/// is below it, read a page at a time ([`Listing`]).
pub fn entries<'a>(
    listing: &'a dyn Listing,
    from: Bound<&[u8]>,
    direction: Direction,
) -> Entries<'a> {
    Entries {
        listing,
        direction,
        from: from.map(<[u8]>::to_vec),
        page: VecDeque::new(),
        more: true,
        size: FIRST_PAGE,
        last: None,
        failed: false,
    }
}

/// The entries of a listing ([`entries`]).
pub struct Entries<'a> {
    listing: &'a dyn Listing,
    direction: Direction,
    /// Where the next page starts.
    from: Bound<Vec<u8>>,
    page: VecDeque<Cow<'a, OwnedNode>>,
    more: bool,
    size: usize,
    last: Option<Vec<u8>>,
    failed: bool,
}

impl<'a> Iterator for Entries<'a> {
    type Item = om_hbam::Result<Cow<'a, OwnedNode>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        if self.page.is_empty() {
            if !self.more {
                return None;
            }
            let mut out = Vec::new();
            let from = self.from.as_ref().map(Vec::as_slice);
            match self.listing.page(from, self.direction, self.size, &mut out) {
                Ok(more) => self.more = more,
                Err(e) => {
                    self.failed = true;
                    return Some(Err(e));
                }
            }
            if out.is_empty() {
                self.more = false;
                return None;
            }
            self.size = (self.size * 2).min(PAGE);
            self.page.extend(out);
        }
        let node = self.page.pop_front()?;
        let key = node.segment.as_slice();
        let start = self.from.as_ref().map(Vec::as_slice);
        let in_order = match self.direction {
            Direction::Forward => {
                (match start {
                    Bound::Included(b) => key >= b,
                    Bound::Excluded(b) => key > b,
                    Bound::Unbounded => true,
                }) && self.last.as_deref().is_none_or(|l| key > l)
            }
            Direction::Backward => {
                (match start {
                    Bound::Included(b) => key <= b,
                    Bound::Excluded(b) => key < b,
                    Bound::Unbounded => true,
                }) && self.last.as_deref().is_none_or(|l| key < l)
            }
        };
        if !in_order {
            self.failed = true;
            return Some(Err(out_of_order()));
        }
        self.last = Some(key.to_vec());
        if self.page.is_empty() {
            self.from = Bound::Excluded(key.to_vec());
        }
        Some(Ok(node))
    }
}

/// The entry under each of `keys` (`None` where the index has none), each
/// looked up by key: what a commit's move of a record reads, changes and
/// keeps in its journal, for the keys of the record's old and new values.
pub fn entries_at<'a, 'k>(
    listing: &'a dyn Listing,
    keys: impl IntoIterator<Item = &'k [u8]>,
) -> om_hbam::Result<BTreeMap<Vec<u8>, Option<Cow<'a, OwnedNode>>>> {
    let mut out = BTreeMap::new();
    for key in keys {
        if out.contains_key(key) {
            continue;
        }
        let mut page = Vec::with_capacity(1);
        listing.page(Bound::Included(key), Direction::Forward, 1, &mut page)?;
        let entry = match page.into_iter().next() {
            Some(node) if node.segment.as_slice() < key => return Err(out_of_order()),
            Some(node) if node.segment == key => Some(node),
            _ => None,
        };
        out.insert(key.to_vec(), entry);
    }
    Ok(out)
}

/// Whether the index holds more than `n` entries, reading at most `n + 1`:
/// whether a move that takes a record out of `n` keys could leave it
/// empty.
pub fn holds_more_than(listing: &dyn Listing, n: usize) -> om_hbam::Result<bool> {
    let mut seen = 0;
    for entry in entries(listing, Bound::Unbounded, Direction::Forward) {
        entry?;
        seen += 1;
        if seen > n {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether the index holds exactly `built`: the same keys, and under each
/// a record list `plain` reads (an entry it cannot read is a difference)
/// equal to the built one. Read in one pass in key order beside `built`,
/// stopping at the first difference, so that an index that disagrees costs
/// only what is read up to it. The index node's own value, which a check
/// also refuses, is its head's.
pub fn holds_exactly(
    listing: &dyn Listing,
    built: &BTreeMap<Vec<u8>, RidList>,
    plain: impl Fn(&OwnedNode) -> Option<RidList>,
) -> om_hbam::Result<bool> {
    let mut wanted = built.iter();
    for entry in entries(listing, Bound::Unbounded, Direction::Forward) {
        let entry = entry?;
        let Some((key, ids)) = wanted.next() else {
            return Ok(false);
        };
        if entry.segment != *key || plain(&entry).as_ref() != Some(ids) {
            return Ok(false);
        }
    }
    Ok(wanted.next().is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::NodeListing;

    fn entry(key: &[u8], ids: &[u64]) -> OwnedNode {
        ids.iter()
            .copied()
            .collect::<RidList>()
            .to_node(key.to_vec())
    }

    fn index(keys: &[(&[u8], &[u64])]) -> OwnedNode {
        OwnedNode::list(vec![0], keys.iter().map(|(k, ids)| entry(k, ids)).collect())
    }

    /// A listing that gives its pages in the wrong order, or the same page
    /// again: every read of it stops with an error.
    struct Broken {
        node: OwnedNode,
        repeat: bool,
    }

    impl Listing for Broken {
        fn page<'a>(
            &'a self,
            _from: Bound<&[u8]>,
            _direction: Direction,
            max: usize,
            out: &mut Vec<Cow<'a, OwnedNode>>,
        ) -> om_hbam::Result<bool> {
            let children = self.node.children.iter().take(max.max(2));
            match self.repeat {
                true => out.extend(children.map(Cow::Borrowed)),
                false => out.extend(children.rev().map(Cow::Borrowed)),
            }
            Ok(true)
        }
    }

    /// Over generated indexes and built ones (equal, or one entry changed,
    /// missing or added), each read through listings of one to four
    /// entries a page answers as the whole-node reads the store's upkeep
    /// makes today: `check_index`'s comparison (the same number of entries,
    /// each equal to the built one), an entry by key (`child`), and the
    /// count against a number.
    #[test]
    fn the_upkeep_reads_answer_as_the_whole_index_did() {
        use crate::source::Capped;
        let mut state = 7u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            state >> 33
        };
        let plain = |n: &OwnedNode| RidList::from_owned(n).ok();
        let mut agreed = 0;
        for _ in 0..500 {
            let keys: BTreeMap<Vec<u8>, Vec<u64>> = (0..next() % 12)
                .map(|_| {
                    let key = (0..1 + next() % 3).map(|_| (next() % 4) as u8).collect();
                    let ids = (0..1 + next() % 3).map(|_| 1 + next() % 9).collect();
                    (key, ids)
                })
                .collect();
            let held =
                OwnedNode::list(vec![0], keys.iter().map(|(k, ids)| entry(k, ids)).collect());
            let mut built: BTreeMap<Vec<u8>, RidList> = keys
                .iter()
                .map(|(k, ids)| (k.clone(), ids.iter().copied().collect()))
                .collect();
            match next() % 4 {
                0 => {}
                1 => {
                    if let Some(first) = built.keys().next().cloned() {
                        built.remove(&first);
                    }
                }
                2 => {
                    built.insert(vec![9], [1].into_iter().collect());
                }
                _ => {
                    if let Some((_, ids)) = built.iter_mut().last() {
                        ids.insert(99);
                    }
                }
            }
            let whole = held.children.len() == built.len()
                && held
                    .children
                    .iter()
                    .all(|e| plain(e).is_some_and(|ids| built.get(&e.segment) == Some(&ids)));
            agreed += usize::from(whole);
            let listing = Capped {
                node: &held,
                cap: 1 + (next() % 4) as usize,
            };
            assert_eq!(holds_exactly(&listing, &built, plain).unwrap(), whole);
            let probes: Vec<Vec<u8>> = (0..4)
                .map(|_| (0..1 + next() % 3).map(|_| (next() % 4) as u8).collect())
                .collect();
            let at = entries_at(&listing, probes.iter().map(Vec::as_slice)).unwrap();
            for probe in &probes {
                assert_eq!(
                    at[probe].as_ref().map(|e| e.as_ref()),
                    held.child(probe),
                    "{probe:02x?}"
                );
            }
            let n = (next() % 14) as usize;
            assert_eq!(
                holds_more_than(&listing, n).unwrap(),
                held.children.len() > n
            );
        }
        assert!(agreed > 50, "{agreed}");
    }

    #[test]
    fn the_upkeep_reads_answer_by_key_and_stop_at_a_broken_listing() {
        let held = index(&[
            (&[1], &[1]),
            (&[1, 5], &[2, 3]),
            (&[1, 9], &[4]),
            (&[2], &[5]),
        ]);
        let listing = NodeListing(&held);
        let at = entries_at(&listing, [&[1, 5][..], &[1, 6], &[2]]).unwrap();
        assert_eq!(
            at.iter()
                .map(|(k, e)| (k.clone(), e.as_ref().map(|e| e.segment.clone())))
                .collect::<Vec<_>>(),
            [
                (vec![1, 5], Some(vec![1, 5])),
                (vec![1, 6], None),
                (vec![2], Some(vec![2])),
            ]
        );
        assert!(holds_more_than(&listing, 3).unwrap());
        assert!(!holds_more_than(&listing, 4).unwrap());
        let plain = |n: &OwnedNode| RidList::from_owned(n).ok();
        let built: BTreeMap<Vec<u8>, RidList> = held
            .children
            .iter()
            .map(|e| (e.segment.clone(), RidList::from_owned(e).unwrap()))
            .collect();
        assert!(holds_exactly(&listing, &built, plain).unwrap());
        let mut fewer = built.clone();
        fewer.remove(&vec![1, 9]);
        assert!(!holds_exactly(&listing, &fewer, plain).unwrap());
        let mut more = built.clone();
        more.insert(vec![3], [6].into_iter().collect());
        assert!(!holds_exactly(&listing, &more, plain).unwrap());
        let mut other = built.clone();
        other.insert(vec![1, 5], [2].into_iter().collect());
        assert!(!holds_exactly(&listing, &other, plain).unwrap());
        let backward: Vec<Vec<u8>> = entries(&listing, Bound::Excluded(&[2]), Direction::Backward)
            .map(|e| e.unwrap().segment.clone())
            .collect();
        assert_eq!(backward, [vec![1, 9], vec![1, 5], vec![1]]);

        for repeat in [false, true] {
            let broken = Broken {
                node: held.clone(),
                repeat,
            };
            // At most the four entries once, then the error; a check never
            // finds that a broken listing agrees.
            let read: Vec<_> = entries(&broken, Bound::Unbounded, Direction::Forward).collect();
            assert!(read.len() <= 5, "{repeat}: {}", read.len());
            assert!(read.last().unwrap().is_err(), "{repeat}");
            assert!(!matches!(holds_exactly(&broken, &built, plain), Ok(true)));
        }
    }
}
