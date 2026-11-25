//! Reading a file's record lists and field indexes (spec
//! `docs/re/query-program.md` 7.1).
//!
//! Under each base table `T`: `[T, 13]` is every record id as one RID list;
//! `[T, 1, field, 0, key]` is the value index and `[T, 1, field, 1, key]` the
//! word index, each key's children being the RID list of the records holding
//! it. Index keys compare as unsigned bytes, and a file holds them in that
//! order, so instructions find keys by binary search and read only the
//! entries their answer needs (spec 7.1, "Reading an index").
//!
//! An index also lists every record whose key it cut short under
//! [`om_collate::TOO_LONG_KEY`] (7.1). That entry is not a value: an
//! [`Index`] leaves it out when it is read, so no lookup, range, prefix,
//! count or full read lands on it (each of those records is also under its
//! cut key), and finds, ranges, duplicates, projections, joins, sorts and
//! value lists read only keys of values.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::{Bound, Range};

use om_hbam::{serial, slot::be_uint, Catalog, Cell, Field, OwnedNode, RidList};

use crate::program::{measure, take_key_string, Size, Work};
use crate::Source;

use crate::QueryError;

/// Key of a table's records.
const RECORDS: u8 = 5;
/// Key of a table's indexes.
const INDEXES: u8 = 1;
/// Under a field's index node: the value index and the word index.
pub const VALUE_INDEX: u8 = 0;
pub const WORD_INDEX: u8 = 1;

/// One field index: its entries in increasing key order, each an index key
/// holding the RID list of the records under it, read where the source
/// keeps them (a hosted file's index is borrowed, not copied).
///
/// Finding a key counts the keys a binary search compares, and reading an
/// entry's records counts the entry, towards the program's work.
#[derive(Debug)]
pub struct Index<'f> {
    node: Option<Cow<'f, OwnedNode>>,
    /// How many of the node's children are entries: all but a last
    /// [`om_collate::TOO_LONG_KEY`], which is no value's key (7.1).
    len: usize,
}

/// What a field without the requested index reads as.
static NO_INDEX: Index<'static> = Index { node: None, len: 0 };

impl<'f> Index<'f> {
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
    fn read(node: Cow<'f, OwnedNode>, work: &Work) -> Result<Self, QueryError> {
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
    pub fn entries(&self) -> &[OwnedNode] {
        self.node
            .as_ref()
            .map_or(&[], |node| &node.children[..self.len])
    }

    pub fn len(&self) -> usize {
        self.entries().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    /// The key of the entry at `at`.
    pub fn key(&self, at: usize) -> &[u8] {
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
    pub fn find(&self, key: &[u8], work: &Work) -> Result<Option<usize>, QueryError> {
        self.find_from(0, key, work).map(|found| found.ok())
    }

    /// The position of `key` among the entries from `from` on: `Ok` where it
    /// is, `Err` where it would go. Looking up keys in increasing order
    /// from the last position walks the index and the keys together.
    pub fn find_from(
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
    pub fn range(
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
    pub fn prefixed(&self, prefix: &[u8], work: &Work) -> Result<Range<usize>, QueryError> {
        let start = self.partition(0, work, |k| k < prefix)?;
        let end = self.partition(start, work, |k| k.starts_with(prefix))?;
        Ok(start..end)
    }

    /// The records under the entry at `at`, counting the entry as read.
    pub fn records(&self, at: usize, work: &Work) -> Result<RidList, QueryError> {
        let entry = &self.entries()[at];
        work.spend(measure(entry).0)?;
        Ok(RidList::from_owned(entry)?)
    }

    /// Add the records under the entries at `positions` to `into`, reading
    /// each entry once (spec 7.1).
    pub fn add_records(
        &self,
        positions: Range<usize>,
        into: &mut RidList,
        work: &Work,
    ) -> Result<(), QueryError> {
        for at in positions {
            *into |= &self.records(at, work)?;
        }
        Ok(())
    }
}

/// Reads indexes from a file, caching each one for the life of a request.
pub struct Indexes<'f> {
    source: &'f dyn Source,
    catalog: Option<Catalog>,
    records: HashMap<Vec<u8>, RidList>,
    /// `None` caches that the field has no such index.
    indexes: HashMap<(Vec<u8>, Vec<u8>, u8), Option<Index<'f>>>,
}

impl<'f> Indexes<'f> {
    pub fn new(source: &'f dyn Source) -> Self {
        Self {
            source,
            catalog: None,
            records: HashMap::new(),
            indexes: HashMap::new(),
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
                let index = Index::read(Cow::Owned(node), work)?;
                self.indexes.insert(id.clone(), Some(index));
            }
        }
        Ok(self.indexes[&id].as_ref().unwrap_or(&NO_INDEX))
    }

    /// An index [`Indexes::index`] has read, as it read it, so that several
    /// can be held at once.
    pub fn cached(&self, table: &[u8], field: &[u8], kind: u8) -> &Index<'f> {
        self.indexes
            .get(&(table.to_vec(), field.to_vec(), kind))
            .and_then(Option::as_ref)
            .unwrap_or(&NO_INDEX)
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
            return Ok(&NO_INDEX);
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
            let index = match self.source.node(&[table, &[INDEXES], field, &[kind]])? {
                Some(node) => Some(Index::read(node, work)?),
                None => None,
            };
            self.indexes.insert(id.clone(), index);
        }
        Ok(self.indexes[&id].as_ref())
    }
}
