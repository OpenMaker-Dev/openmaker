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
//! whole (a file on disk, a tree a host lends), or listed a page at a time
//! from a bound (a hosted file on an engine with pages), so that a lookup
//! reads one entry and a scan the entries it passes, never the index whole.
//! Every instruction asks by key: a lookup, the entries in a range of keys
//! in either direction, or the lowest entry; none needs an entry by its
//! position.
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
use crate::source::{Children, Direction, Listing};
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

/// Reading one page of a listing: a seek, counted as one node however many
/// entries it gives (each entry counts as it is read).
const SEEK: Size = Size { nodes: 1, bytes: 0 };

/// A listed scan's first page, small since many scans stop after a few
/// entries (a lookup's walk, a sort that has keyed its records), and the
/// most later pages ask for, each twice the one before.
const FIRST_PAGE: usize = 8;
const PAGE: usize = 256;

/// One field index: its entries in increasing key order, each an index key
/// holding the RID list of the records under it. Held whole as the source
/// gave it (a hosted file's tree is lent, not copied), or listed from a
/// bound a page at a time ([`Source::children`]).
///
/// On an index held whole, finding a key counts the keys a binary search
/// compares, and the index's keys were counted once when it was read. On a
/// listed one, each page counts as a seek and each key it gives as read.
/// Reading an entry's records counts the entry, either way.
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

/// One entry of an index: its key, holding the RID list of the records
/// under it.
pub struct Entry<'a> {
    node: Cow<'a, OwnedNode>,
}

impl Entry<'_> {
    pub fn key(&self) -> &[u8] {
        &self.node.segment
    }

    /// The records under the entry, counting the entry as read.
    pub fn records(&self, work: &Work) -> Result<RidList, QueryError> {
        work.spend(measure(&self.node).0)?;
        Ok(RidList::from_owned(&self.node)?)
    }
}

/// A source whose listing gave an entry out of the order every listing
/// keeps (or outside the bound it was asked from): it is broken, and the
/// program fails rather than read on.
fn out_of_order() -> QueryError {
    QueryError::File(om_hbam::Error::KeyOrder {
        path: "an index's listing".into(),
    })
}

/// A key a listing gave: one node and its key's bytes.
fn listed(node: &OwnedNode) -> Size {
    Size {
        nodes: 1,
        bytes: node.segment.len(),
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

    /// The entry under `key`, if the index holds it.
    pub fn get(&self, key: &[u8], work: &Work) -> Result<Option<Entry<'_>>, QueryError> {
        match &self.held {
            Held::Absent => Ok(None),
            Held::Whole { node, len } => {
                let entries = Self::held(node, *len);
                let at = Self::partition(entries, 0, work, |k| k < key)?;
                Ok(entries
                    .get(at)
                    .filter(|entry| entry.segment == key)
                    .map(|entry| Entry {
                        node: Cow::Borrowed(entry),
                    }))
            }
            Held::Listed(listing) => {
                work.spend(SEEK)?;
                if key == om_collate::TOO_LONG_KEY {
                    return Ok(None);
                }
                let mut page = Vec::with_capacity(1);
                listing.page(Bound::Included(key), Direction::Forward, 1, &mut page)?;
                let Some(node) = page.into_iter().next() else {
                    return Ok(None);
                };
                work.spend(listed(&node))?;
                if node.segment.as_slice() < key {
                    return Err(out_of_order());
                }
                Ok((node.segment == key).then_some(Entry { node }))
            }
        }
    }

    /// The entry with the lowest key, if the index holds any.
    pub fn first<'a>(&'a self, work: &'a Work) -> Result<Option<Entry<'a>>, QueryError> {
        match &self.held {
            Held::Absent => Ok(None),
            Held::Whole { node, len } => Ok(Self::held(node, *len).first().map(|entry| Entry {
                node: Cow::Borrowed(entry),
            })),
            Held::Listed(_) => self
                .scan(
                    (Bound::Unbounded, Bound::Unbounded),
                    Direction::Forward,
                    work,
                )?
                .next()
                .transpose(),
        }
    }

    /// The entries whose keys lie in `range`, in increasing key order
    /// ([`Direction::Forward`]) or decreasing, each given once.
    pub fn scan<'a>(
        &'a self,
        range: KeyRange<'_>,
        direction: Direction,
        work: &'a Work,
    ) -> Result<Scan<'a>, QueryError> {
        let (low, high) = range;
        let inner = match &self.held {
            Held::Absent => Walk::Held {
                entries: &[],
                positions: 0..0,
                direction,
            },
            Held::Whole { node, len } => {
                let entries = Self::held(node, *len);
                let start = Self::partition(entries, 0, work, |k| !above(k, low))?;
                let end = Self::partition(entries, start, work, |k| below(k, high))?;
                Walk::Held {
                    entries,
                    positions: start..end,
                    direction,
                }
            }
            Held::Listed(listing) => {
                let owned = |b: Bound<&[u8]>| b.map(<[u8]>::to_vec);
                let from = match direction {
                    Direction::Forward => owned(low),
                    Direction::Backward => owned(high),
                };
                Walk::Listed {
                    listing: listing.as_ref(),
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
        };
        Ok(Scan { walk: inner })
    }
}

/// The entries of a range of an [`Index`], in the scan's direction
/// ([`Index::scan`]). A listed scan reads a page at a time, each from just
/// past the last key it gave; a key that does not follow the last one in
/// the scan's direction (or lies outside where the page was asked from)
/// ends the scan with an error, so a broken source can neither repeat
/// entries nor make the scan loop.
pub struct Scan<'a> {
    walk: Walk<'a>,
}

enum Walk<'a> {
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
        work: &'a Work,
        page: VecDeque<Cow<'a, OwnedNode>>,
        /// Where the next page starts.
        from: Bound<Vec<u8>>,
        /// Whether the listing may give more after the last page.
        more: bool,
        size: usize,
        last: Option<Vec<u8>>,
    },
    Done,
}

impl<'a> Iterator for Scan<'a> {
    type Item = Result<Entry<'a>, QueryError>;

    fn next(&mut self) -> Option<Self::Item> {
        let result = self.step();
        if matches!(result, None | Some(Err(_))) {
            self.walk = Walk::Done;
        }
        result
    }
}

impl<'a> Scan<'a> {
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
                Some(Ok(Entry {
                    node: Cow::Borrowed(&entries[at]),
                }))
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
                    match listing.page(
                        from.as_ref().map(Vec::as_slice),
                        *direction,
                        *size,
                        &mut out,
                    ) {
                        Ok(more_after) => *more = more_after,
                        Err(e) => return Some(Err(e.into())),
                    }
                    if out.is_empty() {
                        return None;
                    }
                    *size = (*size * 2).min(PAGE);
                    page.extend(out);
                }
                let node = page.pop_front().expect("a page with entries");
                if let Err(e) = work.spend(listed(&node)) {
                    return Some(Err(e));
                }
                let key = node.segment.as_slice();
                let start = from.as_ref().map(Vec::as_slice);
                let in_order = match direction {
                    Direction::Forward => {
                        above(key, start) && last.as_deref().is_none_or(|l| key > l)
                    }
                    Direction::Backward => {
                        below(key, start) && last.as_deref().is_none_or(|l| key < l)
                    }
                };
                if !in_order {
                    return Some(Err(out_of_order()));
                }
                *last = Some(key.to_vec());
                let past = match direction {
                    Direction::Forward => !below(key, high.as_ref().map(Vec::as_slice)),
                    Direction::Backward => !above(key, low.as_ref().map(Vec::as_slice)),
                };
                if past {
                    return None;
                }
                if key == om_collate::TOO_LONG_KEY {
                    continue;
                }
                return Some(Ok(Entry { node }));
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
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::BTreeSet;
    use std::ops::Bound;

    use om_hbam::{OwnedNode, RidList};

    use super::oracle::OldIndex;
    use super::{Index, KeyRange};
    use crate::program::{Size, Work};
    use crate::source::{Capped, Children, Direction, Listing};

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

    type Read = Vec<(Vec<u8>, RidList)>;

    fn scanned(index: &Index<'_>, range: KeyRange<'_>, direction: Direction, work: &Work) -> Read {
        index
            .scan(range, direction, work)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (e.key().to_vec(), e.records(work).unwrap())
            })
            .collect()
    }

    /// The listed index (pages of one to five entries) and the whole one
    /// answer every lookup, range in either direction, prefix and full
    /// read as the index found by position did, over generated indexes,
    /// the cut keys' entry included (left out by all three).
    #[test]
    fn a_listed_index_answers_as_the_index_found_by_position() {
        let work = Work::new(Size {
            nodes: usize::MAX / 2,
            bytes: usize::MAX / 2,
        });
        for seed in 0..400 {
            let mut next = numbers(seed);
            let count = next() % 40;
            let keys: BTreeSet<Vec<u8>> = (0..count).map(|_| key(&mut next)).collect();
            let entries = keys
                .iter()
                .map(|k| {
                    let ids: RidList = (0..1 + next() % 4).map(|_| 1 + next() % 50).collect();
                    ids.to_node(k.clone())
                })
                .collect();
            let node = OwnedNode::list(vec![0], entries);
            let keys: Vec<Vec<u8>> = keys.into_iter().collect();
            let old = OldIndex::read(Cow::Borrowed(&node), &work).unwrap();
            let whole = Index::whole(Cow::Borrowed(&node), &work).unwrap();
            let cap = 1 + (next() % 5) as usize;
            let listed = Index::read(
                Children::Listed(Box::new(Capped { node: &node, cap })),
                &work,
            )
            .unwrap();
            let at = |at: usize| (old.key(at).to_vec(), old.records(at, &work).unwrap());
            let first = (!old.is_empty()).then(|| at(0));
            for index in [&whole, &listed] {
                let got = index
                    .first(&work)
                    .unwrap()
                    .map(|e| (e.key().to_vec(), e.records(&work).unwrap()));
                assert_eq!(got, first, "seed {seed}: first");
            }
            for _ in 0..30 {
                let probe = match keys.is_empty() || next() % 2 == 0 {
                    true => key(&mut next),
                    false => keys[(next() % keys.len() as u64) as usize].clone(),
                };
                let expected = old.find(&probe, &work).unwrap().map(at);
                for index in [&whole, &listed] {
                    let got = index
                        .get(&probe, &work)
                        .unwrap()
                        .map(|e| (e.key().to_vec(), e.records(&work).unwrap()));
                    assert_eq!(got, expected, "seed {seed}: get {probe:02x?}");
                }
                let (low, high) = (bound(&mut next, &keys), bound(&mut next, &keys));
                let range = (
                    low.as_ref().map(Vec::as_slice),
                    high.as_ref().map(Vec::as_slice),
                );
                let forward: Read = old.range(range, &work).unwrap().map(at).collect();
                let backward: Read = forward.iter().rev().cloned().collect();
                for index in [&whole, &listed] {
                    let what = format!("seed {seed}: {range:02x?}");
                    assert_eq!(
                        scanned(index, range, Direction::Forward, &work),
                        forward,
                        "{what}"
                    );
                    assert_eq!(
                        scanned(index, range, Direction::Backward, &work),
                        backward,
                        "{what}"
                    );
                }
                let prefix = &probe[..(next() % (probe.len() as u64 + 1)) as usize];
                let prefixed: Read = old.prefixed(prefix, &work).unwrap().map(at).collect();
                for index in [&whole, &listed] {
                    let from = (Bound::Included(prefix), Bound::Unbounded);
                    let got: Read = scanned(index, from, Direction::Forward, &work)
                        .into_iter()
                        .take_while(|(k, _)| k.starts_with(prefix))
                        .collect();
                    assert_eq!(got, prefixed, "seed {seed}: prefix {prefix:02x?}");
                }
            }
        }
    }

    /// A listing that gives entries out of key order, or the same page
    /// again and again, never makes a read loop: the lookup or scan that
    /// meets it fails.
    #[test]
    fn a_listing_out_of_order_fails_the_read() {
        struct Broken(OwnedNode, bool);
        impl Listing for Broken {
            fn page<'a>(
                &'a self,
                _from: Bound<&[u8]>,
                _direction: Direction,
                max: usize,
                out: &mut Vec<Cow<'a, OwnedNode>>,
            ) -> om_hbam::Result<bool> {
                let children = self.0.children.iter().take(max.max(2));
                match self.1 {
                    true => out.extend(children.map(Cow::Borrowed)),
                    false => out.extend(children.rev().map(Cow::Borrowed)),
                }
                Ok(true)
            }
        }
        let work = Work::new(Size {
            nodes: 1_000,
            bytes: 1_000_000,
        });
        let entry = |k: &[u8]| [1u64].into_iter().collect::<RidList>().to_node(k.to_vec());
        let node = OwnedNode::list(vec![0], vec![entry(&[1]), entry(&[2]), entry(&[3])]);
        for repeat in [false, true] {
            let index = Index::read(
                Children::Listed(Box::new(Broken(node.clone(), repeat))),
                &work,
            )
            .unwrap();
            let all = (Bound::Unbounded, Bound::Unbounded);
            let read: Vec<_> = index
                .scan(all, Direction::Forward, &work)
                .unwrap()
                .collect();
            // At most the three entries once, then the error.
            assert!(read.len() <= 4 && read.last().unwrap().is_err(), "{repeat}");
        }
        // Asked for `03`, it gives `02` first: before the key asked for.
        let index = Index::read(Children::Listed(Box::new(Broken(node, false))), &work).unwrap();
        assert!(index.get(&[3], &work).is_err());
    }
}
