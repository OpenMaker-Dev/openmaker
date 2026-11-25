//! The reads the upkeep of a stored index needs (docs/server-storage.md
//! 5, "Moving the other readers", item 5), over the listing a find reads an
//! index through ([`Listing`]), so that a host whose writes go through a
//! transaction of pages can keep the indexes without holding one whole:
//!
//! - a commit's moves of a record read and change only the entries under
//!   the keys its old and new values give ([`entries_at`]), and whether the
//!   index holds more entries than the record leaves, which a move that may
//!   empty the index needs ([`holds_more_than`]: keys only, no more than
//!   decide it);
//! - the check that an index holds the keys the current rules give every
//!   record compares the index with one built from the records, in one pass
//!   that stops at the first difference, an entry's records read only when
//!   its key agrees ([`holds_exactly`]);
//! - a sweep reads every entry once, in key order ([`entries`]).
//!
//! Which fields of a table have which indexes is read from the heads of
//! `[T, 01, F]`'s children, not from a listing of entries, and the index
//! node's own value from its head.
//!
//! A listing's keys are read a page at a time, each page from just past the
//! last key it gave, its pages growing to [`PAGE`], and an entry with what
//! is below it only by its key. A key out of the listing's order (or
//! outside where its page was asked from) is an error
//! ([`om_hbam::Error::KeyOrder`]), and so is a page with no key that says
//! more follow, so a broken source can neither repeat an entry, nor make a
//! read loop, nor end one early unnoticed.

use std::borrow::Cow;
use std::collections::{BTreeMap, VecDeque};
use std::ops::Bound;

use om_hbam::{OwnedNode, RidList};

use crate::program::Size;
use crate::source::{broken, out_of_order, Direction, Listing};

/// The most keys a page asks for, after a first one of [`FIRST_PAGE`].
pub const PAGE: usize = 256;
const FIRST_PAGE: usize = 8;

/// Every key of `listing` from `from` on in `direction`, read a page at a
/// time ([`Listing::keys`]), nothing below them.
pub fn keys<'a>(listing: &'a dyn Listing, from: Bound<&[u8]>, direction: Direction) -> Keys<'a> {
    Keys::new(listing, from, direction, usize::MAX)
}

/// The keys of a listing ([`keys`]).
pub struct Keys<'a> {
    listing: &'a dyn Listing,
    direction: Direction,
    /// Where the next page starts.
    from: Bound<Vec<u8>>,
    page: VecDeque<Cow<'a, [u8]>>,
    more: bool,
    size: usize,
    last: Option<Vec<u8>>,
    failed: bool,
    /// How many more keys the reader takes at most: no page asks for more.
    wanted: usize,
}

impl<'a> Keys<'a> {
    fn new(
        listing: &'a dyn Listing,
        from: Bound<&[u8]>,
        direction: Direction,
        wanted: usize,
    ) -> Self {
        Self {
            listing,
            direction,
            from: from.map(<[u8]>::to_vec),
            page: VecDeque::new(),
            more: true,
            size: FIRST_PAGE,
            last: None,
            failed: false,
            wanted,
        }
    }

    fn fail(&mut self, error: om_hbam::Error) -> Option<om_hbam::Result<Cow<'a, [u8]>>> {
        self.failed = true;
        Some(Err(error))
    }
}

impl<'a> Iterator for Keys<'a> {
    type Item = om_hbam::Result<Cow<'a, [u8]>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.wanted == 0 {
            return None;
        }
        if self.page.is_empty() {
            if !self.more {
                return None;
            }
            if let Some(last) = &self.last {
                self.from = Bound::Excluded(last.clone());
            }
            let mut out = Vec::new();
            let from = self.from.as_ref().map(Vec::as_slice);
            let max = self.size.min(self.wanted);
            match self.listing.keys(from, self.direction, max, &mut out) {
                Ok(more) => self.more = more,
                Err(e) => return self.fail(e),
            }
            if out.is_empty() {
                return match self.more {
                    true => self.fail(broken("a page with no key said more follow")),
                    false => None,
                };
            }
            if out.len() > max {
                return self.fail(broken("a page with more keys than asked for"));
            }
            self.size = (self.size * 2).min(PAGE);
            self.page.extend(out);
        }
        let key = self.page.pop_front()?;
        let start = self.from.as_ref().map(Vec::as_slice);
        let in_order = match self.direction {
            Direction::Forward => {
                (match start {
                    Bound::Included(b) => *key >= *b,
                    Bound::Excluded(b) => *key > *b,
                    Bound::Unbounded => true,
                }) && self.last.as_deref().is_none_or(|l| *key > *l)
            }
            Direction::Backward => {
                (match start {
                    Bound::Included(b) => *key <= *b,
                    Bound::Excluded(b) => *key < *b,
                    Bound::Unbounded => true,
                }) && self.last.as_deref().is_none_or(|l| *key < *l)
            }
        };
        if !in_order {
            return self.fail(out_of_order());
        }
        self.last = Some(key.to_vec());
        self.wanted -= 1;
        Some(Ok(key))
    }
}

/// The entry under `key`, which `listing` listed: one it then does not
/// hold is an error (the source broke).
fn listed_entry<'a>(listing: &'a dyn Listing, key: &[u8]) -> om_hbam::Result<Cow<'a, OwnedNode>> {
    listing
        .entry(key, Size::UNLIMITED)?
        .filter(|node| node.segment == key)
        .ok_or_else(|| broken("a key it listed has no entry"))
}

/// Every entry of `listing` from `from` on in `direction`, each with what
/// is below it: its keys a page at a time, and each entry by its key.
pub fn entries<'a>(
    listing: &'a dyn Listing,
    from: Bound<&[u8]>,
    direction: Direction,
) -> impl Iterator<Item = om_hbam::Result<Cow<'a, OwnedNode>>> + 'a {
    let mut keys = keys(listing, from, direction);
    let mut failed = false;
    std::iter::from_fn(move || {
        if failed {
            return None;
        }
        let entry = keys.next()?.and_then(|key| listed_entry(listing, &key));
        failed = entry.is_err();
        Some(entry)
    })
}

/// The entry under each of `keys` (`None` where the index has none), each
/// read by key: what a commit's move of a record reads, changes and keeps
/// in its journal, for the keys of the record's old and new values.
pub fn entries_at<'a, 'k>(
    listing: &'a dyn Listing,
    keys: impl IntoIterator<Item = &'k [u8]>,
) -> om_hbam::Result<BTreeMap<Vec<u8>, Option<Cow<'a, OwnedNode>>>> {
    let mut out = BTreeMap::new();
    for key in keys {
        if out.contains_key(key) {
            continue;
        }
        let entry = match listing.entry(key, Size::UNLIMITED)? {
            Some(node) if node.segment != key => return Err(broken("an entry under another key")),
            entry => entry,
        };
        out.insert(key.to_vec(), entry);
    }
    Ok(out)
}

/// Whether the index holds more than `n` entries, reading at most `n + 1`
/// keys and nothing below them, in pages no larger than the keys still
/// wanted: whether a move that takes a record out of `n` keys could leave
/// it empty.
pub fn holds_more_than(listing: &dyn Listing, n: usize) -> om_hbam::Result<bool> {
    let wanted = n.saturating_add(1);
    let mut seen = 0;
    for key in Keys::new(listing, Bound::Unbounded, Direction::Forward, wanted) {
        key?;
        seen += 1;
    }
    Ok(seen == wanted)
}

/// Whether the index holds exactly `built`: the same keys, and under each
/// a record list `plain` reads (an entry it cannot read is a difference)
/// equal to the built one. Read in one pass in key order beside `built`,
/// stopping at the first difference, an entry read only when its key is
/// the one `built` has there, so that an index that disagrees costs only
/// what is read up to it. The index node's own value, which a check also
/// refuses, is its head's.
pub fn holds_exactly(
    listing: &dyn Listing,
    built: &BTreeMap<Vec<u8>, RidList>,
    plain: impl Fn(&OwnedNode) -> Option<RidList>,
) -> om_hbam::Result<bool> {
    let mut wanted = built.iter();
    for key in keys(listing, Bound::Unbounded, Direction::Forward) {
        let key = key?;
        let Some((want, ids)) = wanted.next() else {
            return Ok(false);
        };
        if *key != **want {
            return Ok(false);
        }
        let entry = listed_entry(listing, &key)?;
        if plain(&entry).as_ref() != Some(ids) {
            return Ok(false);
        }
    }
    Ok(wanted.next().is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{node_entry, node_keys, NodeListing};

    fn entry(key: &[u8], ids: &[u64]) -> OwnedNode {
        ids.iter()
            .copied()
            .collect::<RidList>()
            .to_node(key.to_vec())
    }

    fn index(keys: &[(&[u8], &[u64])]) -> OwnedNode {
        OwnedNode::list(vec![0], keys.iter().map(|(k, ids)| entry(k, ids)).collect())
    }

    /// A listing that breaks its contract in one way: its keys in the wrong
    /// order, the same keys on every page, or a first page of two keys and
    /// then pages with no key that say more follow.
    #[derive(Clone, Copy, Debug)]
    enum Breaks {
        Reversed,
        Repeated,
        Stalled,
        /// Its keys in order, one more a page than asked for.
        Overfull,
    }

    struct Broken {
        node: OwnedNode,
        breaks: Breaks,
    }

    impl Listing for Broken {
        fn keys<'a>(
            &'a self,
            from: Bound<&[u8]>,
            direction: Direction,
            max: usize,
            out: &mut Vec<Cow<'a, [u8]>>,
        ) -> om_hbam::Result<bool> {
            let keys = self
                .node
                .children
                .iter()
                .map(|c| Cow::Borrowed(c.segment.as_slice()));
            match self.breaks {
                Breaks::Reversed => out.extend(keys.rev().take(max.max(2))),
                Breaks::Repeated => out.extend(keys.take(max.max(2))),
                Breaks::Stalled if from == Bound::Unbounded => out.extend(keys.take(2)),
                Breaks::Stalled => {}
                Breaks::Overfull => {
                    return Ok(node_keys(&self.node, from, direction, max + 1, out))
                }
            }
            Ok(true)
        }

        fn entry<'a>(
            &'a self,
            key: &[u8],
            _limit: Size,
        ) -> om_hbam::Result<Option<Cow<'a, OwnedNode>>> {
            Ok(node_entry(&self.node, key))
        }
    }

    /// A listing that notes how many keys each page asks for, and counts
    /// the entries read below its keys.
    struct Noted<'n> {
        node: &'n OwnedNode,
        asked: std::cell::RefCell<Vec<usize>>,
        entries: std::cell::Cell<usize>,
    }

    impl Listing for Noted<'_> {
        fn keys<'a>(
            &'a self,
            from: Bound<&[u8]>,
            direction: Direction,
            max: usize,
            out: &mut Vec<Cow<'a, [u8]>>,
        ) -> om_hbam::Result<bool> {
            self.asked.borrow_mut().push(max);
            Ok(node_keys(self.node, from, direction, max, out))
        }

        fn entry<'a>(
            &'a self,
            key: &[u8],
            _limit: Size,
        ) -> om_hbam::Result<Option<Cow<'a, OwnedNode>>> {
            self.entries.set(self.entries.get() + 1);
            Ok(node_entry(self.node, key))
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

        for breaks in [Breaks::Reversed, Breaks::Repeated, Breaks::Stalled] {
            let broken = Broken {
                node: held.clone(),
                breaks,
            };
            // At most the four entries once, then the error; a check never
            // finds that a broken listing agrees.
            let read: Vec<_> = entries(&broken, Bound::Unbounded, Direction::Forward).collect();
            assert!(read.len() <= 5, "{breaks:?}: {}", read.len());
            assert!(read.last().unwrap().is_err(), "{breaks:?}");
            let check = holds_exactly(&broken, &built, plain);
            assert!(!matches!(check, Ok(true)), "{breaks:?}");
        }
        // A first page that holds every key the built index holds, then a
        // page with no key that says more follow: the index may hold more,
        // so the check fails rather than agree.
        let stalled = Broken {
            node: held.clone(),
            breaks: Breaks::Stalled,
        };
        let first_two: BTreeMap<Vec<u8>, RidList> = built.clone().into_iter().take(2).collect();
        assert!(holds_exactly(&stalled, &first_two, plain).is_err());
        assert!(holds_more_than(&stalled, 2).is_err());
        assert!(holds_more_than(&stalled, 1).unwrap());
        // A page with more keys than asked for: the count fails rather than
        // count a key it did not ask for, as do the check and the sweep
        // (over twelve entries, more than a first page asks for).
        let overfull = Broken {
            node: held.clone(),
            breaks: Breaks::Overfull,
        };
        assert!(holds_more_than(&overfull, 0).is_err());
        assert!(holds_more_than(&overfull, 2).is_err());
        let twelve = index(
            &(1..=12u8)
                .map(|k| (vec![k], vec![u64::from(k)]))
                .collect::<Vec<_>>()
                .iter()
                .map(|(k, ids)| (k.as_slice(), ids.as_slice()))
                .collect::<Vec<_>>(),
        );
        let overfull = Broken {
            node: twelve.clone(),
            breaks: Breaks::Overfull,
        };
        let all: BTreeMap<Vec<u8>, RidList> = twelve
            .children
            .iter()
            .map(|e| (e.segment.clone(), RidList::from_owned(e).unwrap()))
            .collect();
        assert!(holds_exactly(&overfull, &all, plain).is_err());
        let read: Vec<_> = entries(&overfull, Bound::Unbounded, Direction::Forward).collect();
        assert!(read.last().unwrap().is_err());
        // An entry under another key than the one asked for.
        struct Elsewhere(OwnedNode);
        impl Listing for Elsewhere {
            fn keys<'a>(
                &'a self,
                from: Bound<&[u8]>,
                direction: Direction,
                max: usize,
                out: &mut Vec<Cow<'a, [u8]>>,
            ) -> om_hbam::Result<bool> {
                Ok(node_keys(&self.0, from, direction, max, out))
            }

            fn entry<'a>(
                &'a self,
                _key: &[u8],
                _limit: Size,
            ) -> om_hbam::Result<Option<Cow<'a, OwnedNode>>> {
                Ok(self.0.children.first().map(Cow::Borrowed))
            }
        }
        let elsewhere = Elsewhere(held);
        assert!(entries_at(&elsewhere, [&[2][..]]).is_err());
        assert!(
            entries(&elsewhere, Bound::Excluded(&[1]), Direction::Forward)
                .next()
                .unwrap()
                .is_err()
        );
    }

    /// Whether the index holds more than `n` entries reads `n + 1` keys at
    /// most, in pages no larger than the keys still wanted, and no entry
    /// below them.
    #[test]
    fn the_count_reads_no_more_keys_than_decide_it() {
        let held = OwnedNode::list(
            vec![0],
            (0..600u16)
                .map(|n| entry(&n.to_be_bytes(), &[1 + u64::from(n)]))
                .collect(),
        );
        for (n, more) in [
            (0, true),
            (1, true),
            (9, true),
            (300, true),
            (599, true),
            (600, false),
        ] {
            let noted = Noted {
                node: &held,
                asked: Default::default(),
                entries: Default::default(),
            };
            assert_eq!(holds_more_than(&noted, n).unwrap(), more, "{n}");
            let asked = noted.asked.into_inner();
            let mut wanted = n + 1;
            for &max in &asked {
                assert!(max <= wanted && max <= PAGE, "{n}: asked {asked:?}");
                wanted -= max.min(wanted);
            }
            assert_eq!(noted.entries.get(), 0, "{n}");
        }
    }
}
