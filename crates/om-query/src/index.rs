//! Reading a file's record lists and field indexes (spec
//! `docs/re/query-program.md` 7.1).
//!
//! Under each base table `T`: `[T, 13]` is every record id as one RID list;
//! `[T, 1, field, 0, key]` is the value index and `[T, 1, field, 1, key]` the
//! word index, each key's children being the RID list of the records holding
//! it. Index keys compare as unsigned bytes, and a file holds them in that
//! order, so instructions find keys by key and read only the entries their
//! answer needs (spec 7.1, "Reading an index").
//!
//! An index is read as its source gives it ([`Source::children`]): held
//! whole (a file on disk, a tree a host lends), or listed a page of keys at
//! a time from a bound (a hosted file on an engine with pages), so that a
//! lookup reads one key and a scan the keys it passes, an entry's records
//! are read only for the entries an instruction takes, and the index is
//! never read whole. Every instruction asks by key: a lookup, the entries
//! in a range of keys in either direction, the lowest entry, or every
//! entry; none needs an entry by its position. An index held whole is read
//! and counted as before it could be listed: its keys checked once, found
//! by binary search, a run of entries kept as their positions.
//!
//! An index also lists every record whose key it cut short under
//! [`om_collate::TOO_LONG_KEY`] (7.1). That entry is not a value: an
//! [`Index`] leaves it out, so no lookup, range, prefix or full read lands
//! on it (each of those records is also under its cut key), and finds,
//! ranges, duplicates, projections, joins, sorts and value lists read only
//! keys of values.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::ops::{Bound, Range};

use om_hbam::{serial, slot::be_uint, Catalog, Cell, Field, OwnedNode, RidList};

use crate::program::{measure, take_key_string, Size, Work};
use crate::source::{broken, out_of_order, Children, Direction, Listing};
use crate::Source;

use crate::QueryError;

/// Key of a table's records.
const RECORDS: u8 = 5;
/// Key of a table's indexes.
const INDEXES: u8 = 1;
/// Under a field's index node: the value index and the word index.
pub const VALUE_INDEX: u8 = 0;
pub const WORD_INDEX: u8 = 1;

/// A range of index keys: its low bound, then its high one.
pub type KeyRange<'k> = (Bound<&'k [u8]>, Bound<&'k [u8]>);

/// Reading one page of a listing's keys, or one listed entry's records: a
/// seek, counted as one node (each key a page gives counts as it is read,
/// and an entry's records as they are read).
const SEEK: Size = Size { nodes: 1, bytes: 0 };

/// A listed scan's first page, small since many scans stop after a few
/// entries (a lookup's walk, a sort that has keyed its records), and the
/// most later pages ask for, each twice the one before.
const FIRST_PAGE: usize = 8;
const PAGE: usize = 256;

/// One field index: its entries in increasing key order, each an index key
/// holding the RID list of the records under it. Held whole as the source
/// gave it (a hosted file's tree is lent, not copied), or listed from a
/// bound a page of keys at a time ([`Source::children`]), an entry's
/// records read only when an instruction takes them.
///
/// On an index held whole, its keys were counted once when it was read,
/// and finding a key counts the keys a binary search compares, as before
/// indexes could be listed: walks of every entry, and runs between entries
/// already found, search nothing. On a listed one, each page counts as a
/// seek and each key it gives as read. Reading an entry's records counts
/// the entry (and on a listed index a seek), either way.
pub struct Index<'f> {
    held: Held<'f>,
}

enum Held<'f> {
    /// The field has no such index: it reads as one without keys.
    Absent,
    /// The node held whole, its entries in order but for a last
    /// [`om_collate::TOO_LONG_KEY`]: the first `len` are values' keys.
    Whole {
        node: Cow<'f, OwnedNode>,
        len: usize,
    },
    /// Listed a page at a time, in key order.
    Listed(Box<dyn Listing + 'f>),
}

impl std::fmt::Debug for Index<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.held {
            Held::Absent => f.write_str("Index::Absent"),
            Held::Whole { len, .. } => write!(f, "Index::Whole({len} entries)"),
            Held::Listed(_) => f.write_str("Index::Listed"),
        }
    }
}

/// One entry of an index: its key, and under it the RID list of the
/// records holding it, read when [`Entry::records`] asks for them.
pub struct Entry<'a> {
    key: Cow<'a, [u8]>,
    at: At<'a>,
}

#[derive(Clone, Copy)]
enum At<'a> {
    /// An entry of an index held whole, at `position` among its entries.
    Held {
        node: &'a OwnedNode,
        position: usize,
    },
    /// An entry a listing gave the key of.
    Listed(&'a dyn Listing),
}

impl<'a> Entry<'a> {
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// The records under the entry, counting the entry as read. A listed
    /// entry is read only now, a seek, from the listing that gave its key;
    /// one the listing then does not hold is an error (the source broke).
    pub fn records(&self, work: &Work) -> Result<RidList, QueryError> {
        match self.at {
            At::Held { node, .. } => {
                work.spend(measure(node).0)?;
                Ok(RidList::from_owned(node)?)
            }
            At::Listed(listing) => {
                work.spend(SEEK)?;
                let node = listing
                    .entry(&self.key)?
                    .filter(|node| node.segment.as_slice() == &*self.key)
                    .ok_or_else(|| broken("a key it listed has no entry"))?;
                work.spend(measure(&node).0)?;
                Ok(RidList::from_owned(&node)?)
            }
        }
    }

    /// The entry alone, as a span to read ([`Index::records_in`]).
    pub fn span(self) -> Span<'a> {
        Span(match self.at {
            At::Held { position, .. } => Run::Held(position..position + 1),
            At::Listed(_) => Run::One(self),
        })
    }

    /// The entry's position, on an index held whole.
    fn position(&self) -> Option<usize> {
        match self.at {
            At::Held { position, .. } => Some(position),
            At::Listed(_) => None,
        }
    }
}

/// Entries of one index to read once each ([`Index::records_in`]): on an
/// index held whole, a run of positions, found as the index was searched
/// before it could be listed; on a listed one, a range of keys, or an entry
/// a lookup found.
pub struct Span<'a>(Run<'a>);

enum Run<'a> {
    Held(Range<usize>),
    Keys(Bound<Vec<u8>>, Bound<Vec<u8>>),
    One(Entry<'a>),
}

/// A key a listing gave: one node and its key's bytes.
fn listed(key: &[u8]) -> Size {
    Size {
        nodes: 1,
        bytes: key.len(),
    }
}

/// Whether `key` lies at or after `low`.
fn above(key: &[u8], low: Bound<&[u8]>) -> bool {
    match low {
        Bound::Included(b) => key >= b,
        Bound::Excluded(b) => key > b,
        Bound::Unbounded => true,
    }
}

/// Whether `key` lies at or before `high`.
fn below(key: &[u8], high: Bound<&[u8]>) -> bool {
    match high {
        Bound::Included(b) => key <= b,
        Bound::Excluded(b) => key < b,
        Bound::Unbounded => true,
    }
}

impl<'f> Index<'f> {
    /// The index of a field without one: no keys.
    fn absent() -> Self {
        Self { held: Held::Absent }
    }

    /// The index whose entries are `children`: listed as the source lists
    /// them, or held whole ([`Index::whole`]).
    fn read(children: Children<'f>, work: &Work) -> Result<Self, QueryError> {
        match children {
            Children::Listed(listing) => Ok(Self {
                held: Held::Listed(listing),
            }),
            Children::Whole(node) => Self::whole(node, work),
        }
    }

    /// The index whose entries are `node`'s children. Their order is checked
    /// once, counting each key: a file and OpenMaker Server keep index keys
    /// in increasing order (`KeyTree` refuses a file whose keys are not, and
    /// the server inserts in order), but a source is trusted for nothing.
    /// Entries out of order, or with a key repeated, are read as if sorted,
    /// the records of a repeated key merged: that copy counts as reading the
    /// whole index.
    ///
    /// The entry [`om_collate::TOO_LONG_KEY`] (`ff 80`, which lists the
    /// records whose keys the index cut, 7.1) is left out, so that no
    /// lookup, range, prefix, count or full read returns it. No value's key
    /// starts with `ff`, so in an index in order it is the last entry, and
    /// the index is read one entry shorter, still borrowed; anywhere else it
    /// is dropped from the copy.
    fn whole(node: Cow<'f, OwnedNode>, work: &Work) -> Result<Self, QueryError> {
        let entries = &node.children;
        work.spend(Size {
            nodes: entries.len(),
            bytes: entries.iter().map(|e| e.segment.len()).sum(),
        })?;
        let too_long = |e: &OwnedNode| e.segment == om_collate::TOO_LONG_KEY;
        if entries.windows(2).all(|w| w[0].segment < w[1].segment) {
            let len = entries.len() - usize::from(entries.last().is_some_and(too_long));
            if !entries[..len].iter().any(too_long) {
                return Ok(Self {
                    held: Held::Whole { node, len },
                });
            }
        }
        work.spend(measure(&node).0)?;
        let mut node = node.into_owned();
        let mut entries = std::mem::take(&mut node.children);
        entries.retain(|e| !too_long(e));
        entries.sort_by(|a, b| a.segment.cmp(&b.segment));
        let mut sorted: Vec<OwnedNode> = Vec::with_capacity(entries.len());
        let mut entries = entries.into_iter().peekable();
        while let Some(entry) = entries.next() {
            if entries
                .peek()
                .is_some_and(|next| next.segment == entry.segment)
            {
                let mut records = RidList::from_owned(&entry)?;
                while let Some(same) = entries.next_if(|next| next.segment == entry.segment) {
                    records |= &RidList::from_owned(&same)?;
                }
                sorted.push(records.to_node(entry.segment));
            } else {
                sorted.push(entry);
            }
        }
        node.children = sorted;
        let len = node.children.len();
        Ok(Self {
            held: Held::Whole {
                node: Cow::Owned(node),
                len,
            },
        })
    }

    /// The entries of an index held whole, in key order.
    fn held(node: &OwnedNode, len: usize) -> &[OwnedNode] {
        &node.children[..len]
    }

    /// The first position from `from` on in `entries` whose key fails
    /// `before`, which must hold for a prefix of the keys: a binary search,
    /// counting the keys it may compare.
    fn partition(
        entries: &[OwnedNode],
        from: usize,
        work: &Work,
        before: impl Fn(&[u8]) -> bool,
    ) -> Result<usize, QueryError> {
        let entries = &entries[from..];
        work.spend(Size {
            nodes: (usize::BITS - entries.len().leading_zeros()).max(1) as usize,
            bytes: 0,
        })?;
        Ok(from + entries.partition_point(|e| before(&e.segment)))
    }

    /// The entry at `at` of an index held whole.
    fn at(entries: &[OwnedNode], at: usize) -> Entry<'_> {
        let node = &entries[at];
        Entry {
            key: Cow::Borrowed(&node.segment),
            at: At::Held { node, position: at },
        }
    }

    /// The entry under `key`, if the index holds it.
    pub fn get(&self, key: &[u8], work: &Work) -> Result<Option<Entry<'_>>, QueryError> {
        match &self.held {
            Held::Absent => Ok(None),
            Held::Whole { node, len } => {
                let entries = Self::held(node, *len);
                let at = Self::partition(entries, 0, work, |k| k < key)?;
                Ok((entries.get(at).is_some_and(|e| e.segment == key))
                    .then(|| Self::at(entries, at)))
            }
            Held::Listed(listing) => listed_get(listing.as_ref(), key, work),
        }
    }

    /// Lookups of keys given in increasing order ([`Lookups::get`]): on an
    /// index held whole each search starts where the last one ended, so that
    /// the index and the keys are walked together.
    pub fn lookups(&self) -> Lookups<'_> {
        Lookups {
            held: &self.held,
            from: 0,
        }
    }

    /// The entry with the lowest key, if the index holds any.
    pub fn first(&self, work: &Work) -> Result<Option<Entry<'_>>, QueryError> {
        match &self.held {
            Held::Absent => Ok(None),
            Held::Whole { node, len } => {
                let entries = Self::held(node, *len);
                Ok((!entries.is_empty()).then(|| Self::at(entries, 0)))
            }
            Held::Listed(_) => self.entries(work).next().transpose(),
        }
    }

    /// The entries whose keys lie in `range`, in increasing key order
    /// ([`Direction::Forward`]) or decreasing, each given once.
    pub fn scan<'a, 'w>(
        &'a self,
        range: KeyRange<'_>,
        direction: Direction,
        work: &'w Work,
    ) -> Result<Scan<'a, 'w>, QueryError> {
        let walk = match &self.held {
            Held::Absent => Walk::Done,
            Held::Whole { node, len } => {
                let (start, end) = Self::bounds(Self::held(node, *len), range, work)?;
                Walk::Held {
                    entries: Self::held(node, *len),
                    positions: start..end,
                    direction,
                }
            }
            Held::Listed(listing) => Walk::listed(listing.as_ref(), range, direction, work),
        };
        Ok(Scan { walk })
    }

    /// Every entry in increasing key order. On an index held whole, whose
    /// keys were counted when it was read, the walk searches nothing.
    pub fn entries<'a, 'w>(&'a self, work: &'w Work) -> Scan<'a, 'w> {
        let walk = match &self.held {
            Held::Absent => Walk::Done,
            Held::Whole { node, len } => Walk::Held {
                entries: Self::held(node, *len),
                positions: 0..*len,
                direction: Direction::Forward,
            },
            Held::Listed(listing) => Walk::listed(
                listing.as_ref(),
                (Bound::Unbounded, Bound::Unbounded),
                Direction::Forward,
                work,
            ),
        };
        Scan { walk }
    }

    /// The positions where `range` starts and ends in `entries`: two
    /// binary searches.
    fn bounds(
        entries: &[OwnedNode],
        (low, high): KeyRange<'_>,
        work: &Work,
    ) -> Result<(usize, usize), QueryError> {
        let start = Self::partition(entries, 0, work, |k| !above(k, low))?;
        let end = Self::partition(entries, start, work, |k| below(k, high))?;
        Ok((start, end))
    }

    /// The entries whose keys lie in `range`, to read with
    /// [`Index::records_in`]: on an index held whole found now, by the two
    /// searches a scan makes; on a listed one, listed when the span is read.
    pub fn span(&self, range: KeyRange<'_>, work: &Work) -> Result<Span<'_>, QueryError> {
        Ok(Span(match &self.held {
            Held::Absent => Run::Held(0..0),
            Held::Whole { node, len } => {
                let (start, end) = Self::bounds(Self::held(node, *len), range, work)?;
                Run::Held(start..end)
            }
            Held::Listed(_) => Run::Keys(range.0.map(<[u8]>::to_vec), range.1.map(<[u8]>::to_vec)),
        }))
    }

    /// The entries between two this index gave (or its ends), to read with
    /// [`Index::records_in`], with no search: on an index held whole, from
    /// their positions; on a listed one, by their keys.
    pub fn between(&self, low: Bound<&Entry<'_>>, high: Bound<&Entry<'_>>) -> Span<'_> {
        Span(match &self.held {
            Held::Absent => Run::Held(0..0),
            Held::Whole { node, len } => {
                let entries = Self::held(node, *len);
                // An entry of another index (no caller gives one) is placed
                // by its key.
                let place = |e: &Entry<'_>| {
                    e.position().unwrap_or_else(|| {
                        entries.partition_point(|n| n.segment.as_slice() < e.key())
                    })
                };
                let start = match low {
                    Bound::Included(e) => place(e),
                    Bound::Excluded(e) => place(e) + 1,
                    Bound::Unbounded => 0,
                };
                let end = match high {
                    Bound::Included(e) => place(e) + 1,
                    Bound::Excluded(e) => place(e),
                    Bound::Unbounded => *len,
                };
                Run::Held(start..end)
            }
            Held::Listed(_) => {
                let key = |e: &Entry<'_>| e.key().to_vec();
                Run::Keys(low.map(key), high.map(key))
            }
        })
    }

    /// The records under the entries of `spans`, each entry read once
    /// however many spans hold it.
    pub fn records_in(&self, spans: Vec<Span<'_>>, work: &Work) -> Result<RidList, QueryError> {
        let mut found = RidList::new();
        match &self.held {
            Held::Absent => {}
            Held::Whole { node, len } => {
                let entries = Self::held(node, *len);
                let mut found_runs = Vec::with_capacity(spans.len());
                for span in spans {
                    found_runs.push(match span.0 {
                        Run::Held(run) => run,
                        Run::One(entry) => match entry.position() {
                            Some(at) => at..at + 1,
                            None => return Err(QueryError::Malformed("a span of another index")),
                        },
                        Run::Keys(low, high) => {
                            let range = (
                                low.as_ref().map(Vec::as_slice),
                                high.as_ref().map(Vec::as_slice),
                            );
                            let (start, end) = Self::bounds(entries, range, work)?;
                            start..end
                        }
                    });
                }
                // Every run lies within the entries; one that reached past
                // them (an entry of another index) ends at the last.
                let mut runs: Vec<Range<usize>> = found_runs
                    .into_iter()
                    .map(|run| run.start..run.end.min(entries.len()))
                    .filter(|run| !run.is_empty())
                    .collect();
                runs.sort_unstable_by_key(|run| run.start);
                let mut merged: Vec<Range<usize>> = Vec::with_capacity(runs.len());
                for run in runs {
                    match merged.last_mut() {
                        Some(last) if run.start <= last.end => last.end = last.end.max(run.end),
                        _ => merged.push(run),
                    }
                }
                for run in merged {
                    for at in run {
                        found |= &Self::at(entries, at).records(work)?;
                    }
                }
            }
            Held::Listed(_) => {
                let mut ranges = Vec::with_capacity(spans.len());
                for span in spans {
                    ranges.push(match span.0 {
                        Run::Keys(low, high) => (low, high, None),
                        Run::One(entry) => {
                            let key = entry.key().to_vec();
                            (
                                Bound::Included(key.clone()),
                                Bound::Included(key),
                                Some(entry),
                            )
                        }
                        Run::Held(_) => {
                            return Err(QueryError::Malformed("a span of another index"))
                        }
                    });
                }
                ranges.sort_by(|a, b| low_order(&a.0, &b.0));
                let mut merged: Vec<(Bound<Vec<u8>>, Bound<Vec<u8>>, Option<Entry<'_>>)> =
                    Vec::with_capacity(ranges.len());
                for (low, high, entry) in ranges {
                    match merged.last_mut() {
                        Some(last) if reaches(&last.1, &low) => {
                            // An entry found stays found only while the run is
                            // that one key.
                            let same = matches!((&last.0, &last.1, &low, &high),
                                (Bound::Included(a), Bound::Included(b), Bound::Included(c), Bound::Included(d))
                                    if a == b && b == c && c == d);
                            if !same {
                                last.2 = None;
                            }
                            let end = std::mem::replace(&mut last.1, Bound::Unbounded);
                            last.1 = higher(end, high);
                            drop(entry);
                        }
                        _ => merged.push((low, high, entry)),
                    }
                }
                for (low, high, entry) in merged {
                    if let Some(entry) = entry {
                        found |= &entry.records(work)?;
                        continue;
                    }
                    let range = (
                        low.as_ref().map(Vec::as_slice),
                        high.as_ref().map(Vec::as_slice),
                    );
                    for entry in self.scan(range, Direction::Forward, work)? {
                        found |= &entry?.records(work)?;
                    }
                }
            }
        }
        Ok(found)
    }
}

/// [`Index::get`] on a listed index: a seek to the first key at or after
/// `key`, whose entry is not read.
fn listed_get<'a>(
    listing: &'a dyn Listing,
    key: &[u8],
    work: &Work,
) -> Result<Option<Entry<'a>>, QueryError> {
    work.spend(SEEK)?;
    if key == om_collate::TOO_LONG_KEY {
        return Ok(None);
    }
    let mut page = Vec::with_capacity(1);
    let more = listing.keys(Bound::Included(key), Direction::Forward, 1, &mut page)?;
    let Some(next) = page.into_iter().next() else {
        return match more {
            true => Err(broken("a page with no key said more follow").into()),
            false => Ok(None),
        };
    };
    work.spend(listed(&next))?;
    if *next < *key {
        return Err(out_of_order().into());
    }
    Ok((*next == *key).then_some(Entry {
        key: next,
        at: At::Listed(listing),
    }))
}

/// Lookups of keys in increasing order in one index ([`Index::lookups`]).
pub struct Lookups<'a> {
    held: &'a Held<'a>,
    /// On an index held whole, where the next search starts.
    from: usize,
}

impl<'a> Lookups<'a> {
    /// The entry under `key`, if the index holds it. `key` follows every
    /// key looked up before it.
    pub fn get(&mut self, key: &[u8], work: &Work) -> Result<Option<Entry<'a>>, QueryError> {
        match self.held {
            Held::Absent => Ok(None),
            Held::Whole { node, len } => {
                let entries = Index::held(node, *len);
                let at =
                    Index::partition(entries, self.from.min(entries.len()), work, |k| k < key)?;
                let found = entries.get(at).is_some_and(|e| e.segment == key);
                self.from = at + usize::from(found);
                Ok(found.then(|| Index::at(entries, at)))
            }
            Held::Listed(listing) => listed_get(listing.as_ref(), key, work),
        }
    }
}

/// How two low bounds order: the one that lets lower keys in first (an
/// open bound before any key, a key included before the same key
/// excluded).
fn low_order(a: &Bound<Vec<u8>>, b: &Bound<Vec<u8>>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Bound::Unbounded, Bound::Unbounded) => Ordering::Equal,
        (Bound::Unbounded, _) => Ordering::Less,
        (_, Bound::Unbounded) => Ordering::Greater,
        (Bound::Included(x) | Bound::Excluded(x), Bound::Included(y) | Bound::Excluded(y))
            if x != y =>
        {
            x.cmp(y)
        }
        (Bound::Included(_), Bound::Excluded(_)) => Ordering::Less,
        (Bound::Excluded(_), Bound::Included(_)) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

/// Whether some key can lie in a range that starts at `low` and in one
/// that ends at `high`.
fn reaches(high: &Bound<Vec<u8>>, low: &Bound<Vec<u8>>) -> bool {
    match (high, low) {
        (Bound::Unbounded, _) | (_, Bound::Unbounded) => true,
        (Bound::Included(h), Bound::Included(l)) => l <= h,
        (Bound::Included(h) | Bound::Excluded(h), Bound::Included(l) | Bound::Excluded(l)) => l < h,
    }
}

/// The higher of two high bounds.
fn higher(a: Bound<Vec<u8>>, b: Bound<Vec<u8>>) -> Bound<Vec<u8>> {
    match (&a, &b) {
        (Bound::Unbounded, _) => a,
        (_, Bound::Unbounded) => b,
        (Bound::Included(x) | Bound::Excluded(x), Bound::Included(y) | Bound::Excluded(y))
            if x != y =>
        {
            if x > y {
                a
            } else {
                b
            }
        }
        (Bound::Included(_), _) => a,
        _ => b,
    }
}

/// The entries of a range of an [`Index`], in the scan's direction
/// ([`Index::scan`], [`Index::entries`]). A listed scan reads a page of keys
/// at a time, each from just past the last key it gave, and an entry's
/// records only when they are asked for; a key that does not follow the
/// last one in the scan's direction (or lies outside where the page was
/// asked from), or a page with no key that says more follow, ends the scan
/// with an error, so a broken source can neither repeat entries, nor make
/// the scan loop, nor end it early unnoticed.
pub struct Scan<'a, 'w> {
    walk: Walk<'a, 'w>,
}

enum Walk<'a, 'w> {
    Held {
        entries: &'a [OwnedNode],
        positions: Range<usize>,
        direction: Direction,
    },
    Listed {
        listing: &'a dyn Listing,
        low: Bound<Vec<u8>>,
        high: Bound<Vec<u8>>,
        direction: Direction,
        work: &'w Work,
        page: VecDeque<Cow<'a, [u8]>>,
        /// Where the next page starts.
        from: Bound<Vec<u8>>,
        /// Whether the listing may give more after the last page.
        more: bool,
        size: usize,
        last: Option<Vec<u8>>,
    },
    Done,
}

impl<'a, 'w> Walk<'a, 'w> {
    fn listed(
        listing: &'a dyn Listing,
        (low, high): KeyRange<'_>,
        direction: Direction,
        work: &'w Work,
    ) -> Self {
        let owned = |b: Bound<&[u8]>| b.map(<[u8]>::to_vec);
        let from = match direction {
            Direction::Forward => owned(low),
            Direction::Backward => owned(high),
        };
        Walk::Listed {
            listing,
            low: owned(low),
            high: owned(high),
            direction,
            work,
            page: VecDeque::new(),
            from,
            more: true,
            size: FIRST_PAGE,
            last: None,
        }
    }
}

impl<'a> Iterator for Scan<'a, '_> {
    type Item = Result<Entry<'a>, QueryError>;

    fn next(&mut self) -> Option<Self::Item> {
        let result = self.step();
        if matches!(result, None | Some(Err(_))) {
            self.walk = Walk::Done;
        }
        result
    }
}

impl<'a> Scan<'a, '_> {
    fn step(&mut self) -> Option<Result<Entry<'a>, QueryError>> {
        match &mut self.walk {
            Walk::Done => None,
            Walk::Held {
                entries,
                positions,
                direction,
            } => {
                let at = match direction {
                    Direction::Forward => positions.next(),
                    Direction::Backward => positions.next_back(),
                }?;
                Some(Ok(Index::at(entries, at)))
            }
            Walk::Listed {
                listing,
                low,
                high,
                direction,
                work,
                page,
                from,
                more,
                size,
                last,
            } => loop {
                if page.is_empty() {
                    if !*more {
                        return None;
                    }
                    if let Some(last) = last {
                        *from = Bound::Excluded(last.clone());
                    }
                    if let Err(e) = work.spend(SEEK) {
                        return Some(Err(e));
                    }
                    let mut out = Vec::new();
                    match listing.keys(
                        from.as_ref().map(Vec::as_slice),
                        *direction,
                        *size,
                        &mut out,
                    ) {
                        Ok(more_after) => *more = more_after,
                        Err(e) => return Some(Err(e.into())),
                    }
                    if out.is_empty() {
                        return match *more {
                            true => Some(Err(broken("a page with no key said more follow").into())),
                            false => None,
                        };
                    }
                    *size = (*size * 2).min(PAGE);
                    page.extend(out);
                }
                let key = page.pop_front().expect("a page with keys");
                if let Err(e) = work.spend(listed(&key)) {
                    return Some(Err(e));
                }
                let start = from.as_ref().map(Vec::as_slice);
                let in_order = match direction {
                    Direction::Forward => {
                        above(&key, start) && last.as_deref().is_none_or(|l| *key > *l)
                    }
                    Direction::Backward => {
                        below(&key, start) && last.as_deref().is_none_or(|l| *key < *l)
                    }
                };
                if !in_order {
                    return Some(Err(out_of_order().into()));
                }
                *last = Some(key.to_vec());
                let past = match direction {
                    Direction::Forward => !below(&key, high.as_ref().map(Vec::as_slice)),
                    Direction::Backward => !above(&key, low.as_ref().map(Vec::as_slice)),
                };
                if past {
                    return None;
                }
                if *key == om_collate::TOO_LONG_KEY {
                    continue;
                }
                return Some(Ok(Entry {
                    key,
                    at: At::Listed(*listing),
                }));
            },
        }
    }
}

/// Reads indexes from a file, caching each one for the life of a request.
pub struct Indexes<'f> {
    source: &'f dyn Source,
    catalog: Option<Catalog>,
    records: HashMap<Vec<u8>, RidList>,
    /// `None` caches that the field has no such index.
    indexes: HashMap<(Vec<u8>, Vec<u8>, u8), Option<Index<'f>>>,
    /// What a field without the requested index reads as.
    no_index: Index<'f>,
}

impl<'f> Indexes<'f> {
    pub fn new(source: &'f dyn Source) -> Self {
        Self {
            source,
            catalog: None,
            records: HashMap::new(),
            indexes: HashMap::new(),
            no_index: Index::absent(),
        }
    }

    /// The data being queried.
    pub fn source(&self) -> &'f dyn Source {
        self.source
    }

    fn catalog(&mut self) -> Result<&Catalog, QueryError> {
        if self.catalog.is_none() {
            self.catalog = Some(self.source.catalog()?);
        }
        Ok(self.catalog.as_ref().expect("just loaded"))
    }

    /// Whether `table` is a base table of the file.
    pub fn table_exists(&mut self, table: &[u8]) -> Result<bool, QueryError> {
        let Some(id) = be_uint(table) else {
            return Ok(false);
        };
        Ok(self.catalog()?.table(id).is_some())
    }

    /// The base table `table` names: itself when it is a base table, the
    /// occurrence's base table when it is a table occurrence (`d0 00 xx`;
    /// FileMaker Pro names occurrences in some instructions). An
    /// occurrence's definition (`[3, 17, 5, occ, 2]`) starts `02 01 00` and
    /// then the base table as a key string (`03 02 80 02`), in test project
    /// and the FileMaker Server samples.
    pub fn base_table(&mut self, table: &[u8]) -> Result<Option<Vec<u8>>, QueryError> {
        if self.table_exists(table)? {
            return Ok(Some(table.to_vec()));
        }
        let Some(id) = be_uint(table) else {
            return Ok(None);
        };
        let base = self
            .catalog()?
            .table_occurrences
            .iter()
            .find(|o| o.id == id)
            .and_then(|o| o.definition.get(3..))
            .and_then(|key| take_key_string(key).ok())
            .and_then(|(segments, _)| match segments.as_slice() {
                [segment] => Some(segment.clone()),
                _ => None,
            });
        match base {
            Some(base) if self.table_exists(&base)? => Ok(Some(base)),
            _ => Ok(None),
        }
    }

    /// Whether `field` is a field of base table `table`.
    pub fn field_exists(&mut self, table: &[u8], field: &[u8]) -> Result<bool, QueryError> {
        let (Some(table), Some(field)) = (be_uint(table), be_uint(field)) else {
            return Ok(false);
        };
        Ok(self
            .catalog()?
            .table(table)
            .is_some_and(|t| t.field(field).is_some()))
    }

    /// The definition of `field` in `table`.
    pub fn field(&mut self, table: &[u8], field: &[u8]) -> Result<Option<Field>, QueryError> {
        let (Some(table), Some(field)) = (be_uint(table), be_uint(field)) else {
            return Ok(None);
        };
        Ok(self
            .catalog()?
            .table(table)
            .and_then(|t| t.field(field))
            .cloned())
    }

    /// One record's stored value of a text field, `None` when it stores none.
    /// Reading it counts towards the program's work.
    pub fn stored_text(
        &mut self,
        work: &Work,
        table: &[u8],
        field: &[u8],
        record: u64,
    ) -> Result<Option<String>, QueryError> {
        let record = serial::encode(record);
        let path: [&[u8]; 4] = [table, &[RECORDS], &record, field];
        match self.source.node(&path)? {
            Some(node) => {
                work.spend(measure(&node).0)?;
                Cell::from_owned(&node)
                    .text()
                    .transpose()
                    .map_err(QueryError::File)
            }
            None => Ok(None),
        }
    }

    /// Every record id of `table` (`[T, 13]`, [`Source::record_ids`]).
    pub fn all_records(&mut self, table: &[u8]) -> Result<&RidList, QueryError> {
        if !self.records.contains_key(table) {
            let list = self.source.record_ids(table)?;
            self.records.insert(table.to_vec(), list);
        }
        Ok(&self.records[table])
    }

    /// The value (`VALUE_INDEX`) or word (`WORD_INDEX`) index of `field` in
    /// `table`. A field without it gets it from the source when the source
    /// builds indexes on demand, as FileMaker Server does for the index a
    /// finder reads (spec 7.1; FileMaker Server answers that first finder
    /// from the missing index, with no records, and builds it afterwards).
    /// Otherwise the field reads as an empty index.
    pub fn index(
        &mut self,
        work: &Work,
        table: &[u8],
        field: &[u8],
        kind: u8,
    ) -> Result<&Index<'f>, QueryError> {
        let id = (table.to_vec(), field.to_vec(), kind);
        if self.load(work, table, field, kind)?.is_none() {
            if let Some(node) = self.source.index_on_demand(table, field, kind)? {
                let index = Index::whole(Cow::Owned(node), work)?;
                self.indexes.insert(id.clone(), Some(index));
            }
        }
        Ok(self.indexes[&id].as_ref().unwrap_or(&self.no_index))
    }

    /// An index [`Indexes::index`] has read, as it read it, so that several
    /// can be held at once.
    pub fn cached(&self, table: &[u8], field: &[u8], kind: u8) -> &Index<'f> {
        self.indexes
            .get(&(table.to_vec(), field.to_vec(), kind))
            .and_then(Option::as_ref)
            .unwrap_or(&self.no_index)
    }

    /// The index a finder reads: [`Indexes::index`], except that a source
    /// which answers finders as FileMaker Server does
    /// ([`Source::finders_read_missing_indexes`]) has a missing index built
    /// for later programs while this finder reads it as empty (spec 7.1).
    pub fn finder_index(
        &mut self,
        work: &Work,
        table: &[u8],
        field: &[u8],
        kind: u8,
    ) -> Result<&Index<'f>, QueryError> {
        if self.source.finders_read_missing_indexes()
            && self.load(work, table, field, kind)?.is_none()
        {
            self.source.index_on_demand(table, field, kind)?;
            return Ok(&self.no_index);
        }
        self.index(work, table, field, kind)
    }

    /// Whether `field` in `table` has the given index at all (an indexed
    /// field whose records hold no values has an index with no keys).
    pub fn is_indexed(
        &mut self,
        work: &Work,
        table: &[u8],
        field: &[u8],
        kind: u8,
    ) -> Result<bool, QueryError> {
        Ok(self.load(work, table, field, kind)?.is_some())
    }

    fn load(
        &mut self,
        work: &Work,
        table: &[u8],
        field: &[u8],
        kind: u8,
    ) -> Result<Option<&Index<'f>>, QueryError> {
        let id = (table.to_vec(), field.to_vec(), kind);
        if !self.indexes.contains_key(&id) {
            let index = match self.source.children(&[table, &[INDEXES], field, &[kind]])? {
                Some(children) => Some(Index::read(children, work)?),
                None => None,
            };
            self.indexes.insert(id.clone(), index);
        }
        Ok(self.indexes[&id].as_ref())
    }
}

/// The index as it was before it could be listed: held whole, its entries
/// found by position. Kept as the oracle the listed index is compared with.
#[cfg(test)]
mod oracle {
    use std::borrow::Cow;
    use std::ops::{Bound, Range};

    use om_hbam::{OwnedNode, RidList};

    use crate::program::{measure, Size, Work};
    use crate::QueryError;

    /// One field index: its entries in increasing key order, each an index key
    /// holding the RID list of the records under it, read where the source
    /// keeps them (a hosted file's index is borrowed, not copied).
    ///
    /// Finding a key counts the keys a binary search compares, and reading an
    /// entry's records counts the entry, towards the program's work.
    pub(super) struct OldIndex<'f> {
        node: Option<Cow<'f, OwnedNode>>,
        /// How many of the node's children are entries: all but a last
        /// [`om_collate::TOO_LONG_KEY`], which is no value's key (7.1).
        len: usize,
    }

    impl<'f> OldIndex<'f> {
        /// The index whose entries are `node`'s children. Their order is checked
        /// once, counting each key: a file and OpenMaker Server keep index keys
        /// in increasing order (`KeyTree` refuses a file whose keys are not, and
        /// the server inserts in order), but a source is trusted for nothing.
        /// Entries out of order, or with a key repeated, are read as if sorted,
        /// the records of a repeated key merged: that copy counts as reading the
        /// whole index.
        ///
        /// The entry [`om_collate::TOO_LONG_KEY`] (`ff 80`, which lists the
        /// records whose keys the index cut, 7.1) is left out, so that no
        /// lookup, range, prefix, count or full read returns it. No value's key
        /// starts with `ff`, so in an index in order it is the last entry, and
        /// the index is read one entry shorter, still borrowed; anywhere else it
        /// is dropped from the copy.
        pub(super) fn read(node: Cow<'f, OwnedNode>, work: &Work) -> Result<Self, QueryError> {
            let entries = &node.children;
            work.spend(Size {
                nodes: entries.len(),
                bytes: entries.iter().map(|e| e.segment.len()).sum(),
            })?;
            let too_long = |e: &OwnedNode| e.segment == om_collate::TOO_LONG_KEY;
            if entries.windows(2).all(|w| w[0].segment < w[1].segment) {
                let len = entries.len() - usize::from(entries.last().is_some_and(too_long));
                if !entries[..len].iter().any(too_long) {
                    return Ok(Self {
                        node: Some(node),
                        len,
                    });
                }
            }
            work.spend(measure(&node).0)?;
            let mut node = node.into_owned();
            let mut entries = std::mem::take(&mut node.children);
            entries.retain(|e| !too_long(e));
            entries.sort_by(|a, b| a.segment.cmp(&b.segment));
            let mut sorted: Vec<OwnedNode> = Vec::with_capacity(entries.len());
            let mut entries = entries.into_iter().peekable();
            while let Some(entry) = entries.next() {
                if entries
                    .peek()
                    .is_some_and(|next| next.segment == entry.segment)
                {
                    let mut records = RidList::from_owned(&entry)?;
                    while let Some(same) = entries.next_if(|next| next.segment == entry.segment) {
                        records |= &RidList::from_owned(&same)?;
                    }
                    sorted.push(records.to_node(entry.segment));
                } else {
                    sorted.push(entry);
                }
            }
            node.children = sorted;
            let len = node.children.len();
            Ok(Self {
                node: Some(Cow::Owned(node)),
                len,
            })
        }

        /// The entries in key order.
        pub(super) fn entries(&self) -> &[OwnedNode] {
            self.node
                .as_ref()
                .map_or(&[], |node| &node.children[..self.len])
        }

        pub(super) fn is_empty(&self) -> bool {
            self.entries().is_empty()
        }

        /// The key of the entry at `at`.
        pub(super) fn key(&self, at: usize) -> &[u8] {
            &self.entries()[at].segment
        }

        /// The first position from `from` on whose key fails `before`, which
        /// must hold for a prefix of the keys: a binary search, counting the
        /// keys it may compare.
        fn partition(
            &self,
            from: usize,
            work: &Work,
            before: impl Fn(&[u8]) -> bool,
        ) -> Result<usize, QueryError> {
            let entries = &self.entries()[from..];
            work.spend(Size {
                nodes: (usize::BITS - entries.len().leading_zeros()).max(1) as usize,
                bytes: 0,
            })?;
            Ok(from + entries.partition_point(|e| before(&e.segment)))
        }

        /// The position of `key`, if the index holds it.
        pub(super) fn find(&self, key: &[u8], work: &Work) -> Result<Option<usize>, QueryError> {
            self.find_from(0, key, work).map(|found| found.ok())
        }

        /// The position of `key` among the entries from `from` on: `Ok` where it
        /// is, `Err` where it would go. Looking up keys in increasing order
        /// from the last position walks the index and the keys together.
        pub(super) fn find_from(
            &self,
            from: usize,
            key: &[u8],
            work: &Work,
        ) -> Result<Result<usize, usize>, QueryError> {
            let at = self.partition(from, work, |k| k < key)?;
            Ok(match self.entries().get(at) {
                Some(entry) if entry.segment == key => Ok(at),
                _ => Err(at),
            })
        }

        /// The positions of the keys between `low` and `high`.
        pub(super) fn range(
            &self,
            (low, high): (Bound<&[u8]>, Bound<&[u8]>),
            work: &Work,
        ) -> Result<Range<usize>, QueryError> {
            let start = self.partition(0, work, |k| match low {
                Bound::Included(b) => k < b,
                Bound::Excluded(b) => k <= b,
                Bound::Unbounded => false,
            })?;
            let end = self.partition(start, work, |k| match high {
                Bound::Included(b) => k <= b,
                Bound::Excluded(b) => k < b,
                Bound::Unbounded => true,
            })?;
            Ok(start..end)
        }

        /// The positions of the keys that start with `prefix`, which follow one
        /// another in key order.
        pub(super) fn prefixed(
            &self,
            prefix: &[u8],
            work: &Work,
        ) -> Result<Range<usize>, QueryError> {
            let start = self.partition(0, work, |k| k < prefix)?;
            let end = self.partition(start, work, |k| k.starts_with(prefix))?;
            Ok(start..end)
        }

        /// The records under the entry at `at`, counting the entry as read.
        pub(super) fn records(&self, at: usize, work: &Work) -> Result<RidList, QueryError> {
            let entry = &self.entries()[at];
            work.spend(measure(entry).0)?;
            Ok(RidList::from_owned(entry)?)
        }

        /// The records under the entries at `ranges`, each entry read once
        /// (JoinProjection's and LookupValue's `records_at`).
        pub(super) fn records_at(
            &self,
            mut ranges: Vec<Range<usize>>,
            work: &Work,
        ) -> Result<RidList, QueryError> {
            ranges.retain(|r| !r.is_empty());
            ranges.sort_unstable_by_key(|r| r.start);
            let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
            for range in ranges {
                match merged.last_mut() {
                    Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                    _ => merged.push(range),
                }
            }
            let mut found = RidList::new();
            for range in merged {
                for at in range {
                    found |= &self.records(at, work)?;
                }
            }
            Ok(found)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::ops::{Bound, Range};

    use om_hbam::{OwnedNode, RidList};

    use super::oracle::OldIndex;
    use super::{Entry, Index, KeyRange, Span};
    use crate::program::{Size, Work};
    use crate::source::{node_entry, node_keys, Capped, Children, Direction, Listing};

    /// A seeded source of numbers (splitmix64).
    fn numbers(seed: u64) -> impl FnMut() -> u64 {
        let mut state = seed;
        move || {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
    }

    /// A key of one to four bytes from a few values, so that keys share
    /// prefixes; now and then the cut keys' entry `ff 80` or a key past it.
    fn key(next: &mut dyn FnMut() -> u64) -> Vec<u8> {
        match next() % 20 {
            0 => om_collate::TOO_LONG_KEY.to_vec(),
            1 => vec![0xff, 0x80, 0x01],
            _ => {
                const BYTES: [u8; 5] = [0x00, 0x01, 0x10, 0x20, 0xfe];
                let len = 1 + next() % 4;
                (0..len).map(|_| BYTES[(next() % 5) as usize]).collect()
            }
        }
    }

    fn bound(next: &mut dyn FnMut() -> u64, keys: &[Vec<u8>]) -> Bound<Vec<u8>> {
        let at = |next: &mut dyn FnMut() -> u64| match keys.is_empty() || next() % 2 == 0 {
            true => key(next),
            false => keys[(next() % keys.len() as u64) as usize].clone(),
        };
        match next() % 4 {
            0 => Bound::Unbounded,
            1 => Bound::Excluded(at(next)),
            _ => Bound::Included(at(next)),
        }
    }

    fn unlimited() -> Work {
        Work::new(Size {
            nodes: usize::MAX / 2,
            bytes: usize::MAX / 2,
        })
    }

    type Read = Vec<(Vec<u8>, RidList)>;

    fn read(entry: Entry<'_>, work: &Work) -> (Vec<u8>, RidList) {
        (entry.key().to_vec(), entry.records(work).unwrap())
    }

    fn scanned(index: &Index<'_>, range: KeyRange<'_>, direction: Direction, work: &Work) -> Read {
        index
            .scan(range, direction, work)
            .unwrap()
            .map(|e| read(e.unwrap(), work))
            .collect()
    }

    /// Each read answered by the index found by position, as it was before
    /// indexes could be listed: its answer and the work it counted.
    struct Reads<'o, 'f> {
        old: &'o OldIndex<'f>,
    }

    impl Reads<'_, '_> {
        fn at(&self, at: usize, work: &Work) -> (Vec<u8>, RidList) {
            (
                self.old.key(at).to_vec(),
                self.old.records(at, work).unwrap(),
            )
        }
    }

    /// `read` with work of its own, and what it counted.
    fn counted<T>(read: impl FnOnce(&Work) -> T) -> (T, Size) {
        let work = unlimited();
        let answer = read(&work);
        (answer, work.done())
    }

    /// Over generated indexes, the cut keys' entry among them now and then,
    /// the index held whole and the index listed (pages of one to five
    /// keys) answer every read as the index found by position did: a
    /// lookup, lookups of keys in increasing order, the first entry, a
    /// range in either direction, a prefix, every entry, and the records
    /// under spans (a range, entries found by lookups, the runs between
    /// found entries). Held whole, each also counts exactly the work the
    /// index found by position counted, reading the index included.
    #[test]
    fn a_listed_index_answers_as_the_index_found_by_position() {
        for seed in 0..400 {
            let mut next = numbers(seed);
            let count = next() % 40;
            let keys: BTreeSet<Vec<u8>> = (0..count).map(|_| key(&mut next)).collect();
            let entries = keys
                .iter()
                .map(|k| {
                    let ids: RidList = (0..next() % 4).map(|_| 1 + next() % 50).collect();
                    ids.to_node(k.clone())
                })
                .collect();
            let node = OwnedNode::list(vec![0], entries);
            let keys: Vec<Vec<u8>> = keys.into_iter().collect();
            let (old, old_read) = counted(|w| OldIndex::read(Cow::Borrowed(&node), w).unwrap());
            let (whole, whole_read) = counted(|w| Index::whole(Cow::Borrowed(&node), w).unwrap());
            assert_eq!(whole_read, old_read, "seed {seed}: reading the index");
            let reads = Reads { old: &old };
            let cap = 1 + (next() % 5) as usize;
            let listed = Index::read(
                Children::Listed(Box::new(Capped { node: &node, cap })),
                &unlimited(),
            )
            .unwrap();
            let both = [&whole, &listed];
            // Each read: the old answer and work, then each index's.
            let check = |what: &str,
                         expected: &dyn Fn(&Work) -> String,
                         got: &dyn Fn(&Index<'_>, &Work) -> String| {
                let expected = counted(expected);
                for (index, name) in both.iter().zip(["whole", "listed"]) {
                    let answer = counted(|w| got(index, w));
                    match name {
                        "whole" => assert_eq!(answer, expected, "seed {seed}: {what}, {name}"),
                        _ => assert_eq!(answer.0, expected.0, "seed {seed}: {what}, {name}"),
                    }
                }
            };
            check(
                "first",
                &|w| format!("{:?}", (!old.is_empty()).then(|| reads.at(0, w))),
                &|index, w| format!("{:?}", index.first(w).unwrap().map(|e| read(e, w))),
            );
            check(
                "every entry",
                &|w| {
                    format!(
                        "{:?}",
                        (0..old.entries().len())
                            .map(|at| reads.at(at, w))
                            .collect::<Read>()
                    )
                },
                &|index, w| {
                    let all: Read = index.entries(w).map(|e| read(e.unwrap(), w)).collect();
                    format!("{all:?}")
                },
            );
            let mut probes: Vec<Vec<u8>> = (0..1 + next() % 6)
                .map(|_| match keys.is_empty() || next() % 2 == 0 {
                    true => key(&mut next),
                    false => keys[(next() % keys.len() as u64) as usize].clone(),
                })
                .collect();
            probes.sort();
            probes.dedup();
            check(
                &format!("lookups {probes:02x?}"),
                &|w| {
                    let mut from = 0;
                    let found: Vec<_> = probes
                        .iter()
                        .map(|p| match old.find_from(from, p, w).unwrap() {
                            Ok(at) => {
                                from = at + 1;
                                Some(reads.at(at, w))
                            }
                            Err(at) => {
                                from = at;
                                None
                            }
                        })
                        .collect();
                    format!("{found:?}")
                },
                &|index, w| {
                    let mut lookups = index.lookups();
                    let found: Vec<_> = probes
                        .iter()
                        .map(|p| lookups.get(p, w).unwrap().map(|e| read(e, w)))
                        .collect();
                    format!("{found:?}")
                },
            );
            check(
                &format!("spans of lookups {probes:02x?}"),
                &|w| {
                    let ranges = probes
                        .iter()
                        .filter_map(|p| old.find(p, w).unwrap())
                        .map(|at| at..at + 1)
                        .collect();
                    format!("{:?}", old.records_at(ranges, w).unwrap())
                },
                &|index, w| {
                    let spans: Vec<Span<'_>> = probes
                        .iter()
                        .filter_map(|p| index.get(p, w).unwrap())
                        .map(Entry::span)
                        .collect();
                    format!("{:?}", index.records_in(spans, w).unwrap())
                },
            );
            // The runs around a key found (`≠` of one key) from the lowest
            // entry or past it.
            let skip = next() % 2 == 0;
            check(
                &format!(
                    "runs around {:02x?}, skipping the lowest: {skip}",
                    probes[0]
                ),
                &|w| {
                    if old.is_empty() {
                        return format!("{:?}", RidList::new());
                    }
                    let first = usize::from(skip);
                    let ranges: Vec<Range<usize>> = match old.find(&probes[0], w).unwrap() {
                        Some(at) => vec![first..at, at + 1..old.entries().len()],
                        None => vec![first..old.entries().len()],
                    };
                    format!("{:?}", old.records_at(ranges, w).unwrap())
                },
                &|index, w| {
                    let Some(lowest) = index.first(w).unwrap() else {
                        return format!("{:?}", RidList::new());
                    };
                    let from = match skip {
                        true => Bound::Excluded(&lowest),
                        false => Bound::Unbounded,
                    };
                    let spans = match index.get(&probes[0], w).unwrap() {
                        Some(entry) => vec![
                            index.between(from, Bound::Excluded(&entry)),
                            index.between(Bound::Excluded(&entry), Bound::Unbounded),
                        ],
                        None => vec![index.between(from, Bound::Unbounded)],
                    };
                    format!("{:?}", index.records_in(spans, w).unwrap())
                },
            );
            for _ in 0..30 {
                let probe = match keys.is_empty() || next() % 2 == 0 {
                    true => key(&mut next),
                    false => keys[(next() % keys.len() as u64) as usize].clone(),
                };
                check(
                    &format!("get {probe:02x?}"),
                    &|w| {
                        format!(
                            "{:?}",
                            old.find(&probe, w).unwrap().map(|at| reads.at(at, w))
                        )
                    },
                    &|index, w| format!("{:?}", index.get(&probe, w).unwrap().map(|e| read(e, w))),
                );
                let (low, high) = (bound(&mut next, &keys), bound(&mut next, &keys));
                let range = (
                    low.as_ref().map(Vec::as_slice),
                    high.as_ref().map(Vec::as_slice),
                );
                for direction in [Direction::Forward, Direction::Backward] {
                    check(
                        &format!("{range:02x?} {direction:?}"),
                        &|w| {
                            let positions = old.range(range, w).unwrap();
                            let read: Read = match direction {
                                Direction::Forward => positions.map(|at| reads.at(at, w)).collect(),
                                Direction::Backward => {
                                    positions.rev().map(|at| reads.at(at, w)).collect()
                                }
                            };
                            format!("{read:?}")
                        },
                        &|index, w| format!("{:?}", scanned(index, range, direction, w)),
                    );
                }
                check(
                    &format!("span {range:02x?}"),
                    &|w| {
                        let ranges = vec![old.range(range, w).unwrap()];
                        format!("{:?}", old.records_at(ranges, w).unwrap())
                    },
                    &|index, w| {
                        let spans = vec![index.span(range, w).unwrap()];
                        format!("{:?}", index.records_in(spans, w).unwrap())
                    },
                );
                let prefix = &probe[..(next() % (probe.len() as u64 + 1)) as usize];
                check(
                    &format!("prefix {prefix:02x?}"),
                    &|w| {
                        let read: Read = old
                            .prefixed(prefix, w)
                            .unwrap()
                            .map(|at| reads.at(at, w))
                            .collect();
                        format!("{read:?}")
                    },
                    &|index, w| {
                        let from = (Bound::Included(prefix), Bound::Unbounded);
                        let mut read = Read::new();
                        for entry in index.scan(from, Direction::Forward, w).unwrap() {
                            let entry = entry.unwrap();
                            if !entry.key().starts_with(prefix) {
                                break;
                            }
                            read.push((entry.key().to_vec(), entry.records(w).unwrap()));
                        }
                        format!("{read:?}")
                    },
                );
            }
        }
    }

    /// A listing that breaks its contract in one way ([`Listing`]).
    #[derive(Clone, Copy, Debug)]
    enum Breaks {
        /// Its keys in decreasing order.
        Reversed,
        /// The same first keys again on every page.
        Repeated,
        /// A first page of two keys, then pages with no key that say
        /// more follow.
        Stalled,
        /// Its keys, but no entry under any of them.
        Empty,
    }

    struct Broken(OwnedNode, Breaks);

    impl Listing for Broken {
        fn keys<'a>(
            &'a self,
            from: Bound<&[u8]>,
            direction: Direction,
            max: usize,
            out: &mut Vec<Cow<'a, [u8]>>,
        ) -> om_hbam::Result<bool> {
            let keys = self
                .0
                .children
                .iter()
                .map(|c| Cow::Borrowed(c.segment.as_slice()));
            match self.1 {
                Breaks::Reversed => out.extend(keys.rev().take(max.max(2))),
                Breaks::Repeated => out.extend(keys.take(max.max(2))),
                Breaks::Stalled if from == Bound::Unbounded => out.extend(keys.take(2)),
                Breaks::Stalled => {}
                Breaks::Empty => return Ok(node_keys(&self.0, from, direction, max, out)),
            }
            Ok(true)
        }

        fn entry<'a>(&'a self, key: &[u8]) -> om_hbam::Result<Option<Cow<'a, OwnedNode>>> {
            match self.1 {
                Breaks::Empty => Ok(None),
                _ => Ok(node_entry(&self.0, key)),
            }
        }
    }

    /// A listing that gives keys out of order, the same page again and
    /// again, a page with no key that says more follow, or a key without
    /// an entry never makes a read loop, nor ends one early as if the
    /// index ended there: the lookup or scan that meets it fails.
    #[test]
    fn a_broken_listing_fails_the_read() {
        let work = Work::new(Size {
            nodes: 1_000,
            bytes: 1_000_000,
        });
        let entry = |k: &[u8]| [1u64].into_iter().collect::<RidList>().to_node(k.to_vec());
        let node = OwnedNode::list(
            vec![0],
            vec![entry(&[1]), entry(&[2]), entry(&[3]), entry(&[4])],
        );
        for breaks in [
            Breaks::Reversed,
            Breaks::Repeated,
            Breaks::Stalled,
            Breaks::Empty,
        ] {
            let index = Index::read(
                Children::Listed(Box::new(Broken(node.clone(), breaks))),
                &work,
            )
            .unwrap();
            let all = (Bound::Unbounded, Bound::Unbounded);
            let read: Vec<_> = index
                .scan(all, Direction::Forward, &work)
                .unwrap()
                .map(|entry| entry.and_then(|e| e.records(&work)))
                .collect();
            // At most the four entries once, then the error.
            assert!(read.len() <= 5, "{breaks:?}");
            assert!(read.last().unwrap().is_err(), "{breaks:?}: {read:?}");
            let spans = vec![index.span(all, &work).unwrap()];
            assert!(index.records_in(spans, &work).is_err(), "{breaks:?}");
        }
        // Asked for `03`, it gives `04` first: past the key asked for, so
        // `03` reads as missing; `04` is the key, but `01` (before it) is
        // what it gives for `05`.
        let reversed = Index::read(
            Children::Listed(Box::new(Broken(node.clone(), Breaks::Reversed))),
            &work,
        )
        .unwrap();
        assert!(reversed.get(&[5], &work).is_err());
        // Asked for `03`, a page with no key that says more follow.
        let stalled = Index::read(
            Children::Listed(Box::new(Broken(node, Breaks::Stalled))),
            &work,
        )
        .unwrap();
        assert!(stalled.get(&[3], &work).is_err());
        assert!(stalled.lookups().get(&[3], &work).is_err());
    }

    /// A listing that counts the entries read below its keys.
    struct Counted<'n> {
        listing: Capped<'n>,
        entries: &'n Cell<usize>,
    }

    impl Listing for Counted<'_> {
        fn keys<'a>(
            &'a self,
            from: Bound<&[u8]>,
            direction: Direction,
            max: usize,
            out: &mut Vec<Cow<'a, [u8]>>,
        ) -> om_hbam::Result<bool> {
            self.listing.keys(from, direction, max, out)
        }

        fn entry<'a>(&'a self, key: &[u8]) -> om_hbam::Result<Option<Cow<'a, OwnedNode>>> {
            self.entries.set(self.entries.get() + 1);
            self.listing.entry(key)
        }
    }

    /// On a listed index an entry's records are read only when asked for,
    /// and counted when read: a lookup that misses next to an entry that
    /// lists most records reads none of them, however many such lookups,
    /// and a scan that stops reads the records of the entries it took.
    #[test]
    fn a_listed_index_reads_records_only_where_asked() {
        // Keys `01 n` for the few, then `02`, which lists 10,000 records.
        let mut entries: Vec<OwnedNode> = (0..50u8)
            .map(|n| {
                [1 + u64::from(n)]
                    .into_iter()
                    .collect::<RidList>()
                    .to_node(vec![1, n])
            })
            .collect();
        entries.push((1..=10_000).collect::<RidList>().to_node(vec![2]));
        let node = OwnedNode::list(vec![0], entries);
        let read = Cell::new(0);
        let counted = Counted {
            listing: Capped {
                node: &node,
                cap: 16,
            },
            entries: &read,
        };
        let index = Index::read(Children::Listed(Box::new(counted)), &unlimited()).unwrap();
        let reads = || read.get();
        let work = unlimited();
        let mut lookups = index.lookups();
        for n in 0..1_000u32 {
            // Each just below `02`.
            let key = [&[1, 0xff][..], &n.to_be_bytes()].concat();
            assert!(lookups.get(&key, &work).unwrap().is_none());
        }
        assert_eq!(reads(), 0);
        // A seek and one key, `02`, each.
        assert_eq!(
            work.done(),
            Size {
                nodes: 2_000,
                bytes: 1_000
            }
        );
        let mut taken = 0;
        for entry in index
            .scan(
                (Bound::Unbounded, Bound::Unbounded),
                Direction::Forward,
                &work,
            )
            .unwrap()
            .take(20)
        {
            let entry = entry.unwrap();
            if entry.key()[1] % 3 == 0 {
                entry.records(&work).unwrap();
                taken += 1;
            }
        }
        assert_eq!(reads(), taken);
    }
}
