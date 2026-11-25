//! What a program reads: a file on disk, or a key tree a host keeps in
//! memory and changes as clients write.

use std::borrow::Cow;
use std::ops::Bound;

use om_hbam::{Catalog, HbamFile, OwnedNode, RidList};

/// Key of a table's record-id list.
const RECORD_IDS: u8 = 13;

/// Which way a listing goes: in increasing key order, or in decreasing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

/// A node's children read a page at a time, in key order, from a bound
/// (`[T, 01, F, kind]`'s entries: query-program.md 7.1), so that a reader
/// holds only the children it asks for.
pub trait Listing {
    /// Up to `max` children, each with everything below it, onto `out`:
    /// forward, those at or after an included `from` (after an excluded
    /// one, from the first when unbounded), in strictly increasing key
    /// order; backward, those at or before it (before an excluded one, from
    /// the last when unbounded), in strictly decreasing order. Whether more
    /// may follow the last one given: a listing may give fewer than `max`
    /// and still have more, but gives at least one while any is left.
    fn page<'a>(
        &'a self,
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Cow<'a, OwnedNode>>,
    ) -> om_hbam::Result<bool>;
}

/// A listing of a node held whole that gives at most `cap` entries a
/// page, so that a test's scans cross pages.
#[cfg(test)]
pub(crate) struct Capped<'n> {
    pub node: &'n OwnedNode,
    pub cap: usize,
}

#[cfg(test)]
impl Listing for Capped<'_> {
    fn page<'a>(
        &'a self,
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Cow<'a, OwnedNode>>,
    ) -> om_hbam::Result<bool> {
        let mut page = Vec::new();
        let more = node_page(self.node, from, direction, self.cap, &mut page);
        let more = more || page.len() > max;
        out.extend(page.into_iter().take(max));
        Ok(more)
    }
}

/// A node's children as a source gives them ([`Source::children`]).
pub enum Children<'s> {
    /// The node read whole, its children in the order the source keeps
    /// them, which a reader checks.
    Whole(Cow<'s, OwnedNode>),
    /// Listed a page at a time, in key order ([`Listing`]).
    Listed(Box<dyn Listing + 's>),
}

/// A listing of the children of a node held whole, which are in strictly
/// increasing key order (as a file or a host keeps them): what an index
/// held in memory is read through where a listing is asked for.
pub struct NodeListing<'n>(pub &'n OwnedNode);

impl Listing for NodeListing<'_> {
    fn page<'a>(
        &'a self,
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Cow<'a, OwnedNode>>,
    ) -> om_hbam::Result<bool> {
        Ok(node_page(self.0, from, direction, max, out))
    }
}

/// [`Listing::page`] over the children of `node`, which are in order.
pub(crate) fn node_page<'n>(
    node: &'n OwnedNode,
    from: Bound<&[u8]>,
    direction: Direction,
    max: usize,
    out: &mut Vec<Cow<'n, OwnedNode>>,
) -> bool {
    {
        let children = &node.children;
        let at = |key: &[u8], after: bool| {
            children.partition_point(|c| match after {
                true => c.segment.as_slice() <= key,
                false => c.segment.as_slice() < key,
            })
        };
        match direction {
            Direction::Forward => {
                let start = match from {
                    Bound::Included(key) => at(key, false),
                    Bound::Excluded(key) => at(key, true),
                    Bound::Unbounded => 0,
                };
                let rest = &children[start..];
                out.extend(rest.iter().take(max).map(Cow::Borrowed));
                rest.len() > max
            }
            Direction::Backward => {
                let end = match from {
                    Bound::Included(key) => at(key, true),
                    Bound::Excluded(key) => at(key, false),
                    Bound::Unbounded => children.len(),
                };
                let rest = &children[..end];
                out.extend(rest.iter().rev().take(max).map(Cow::Borrowed));
                rest.len() > max
            }
        }
    }
}

/// The data a program runs against.
pub trait Source {
    /// The file's schema (tables and fields).
    fn catalog(&self) -> om_hbam::Result<Catalog>;

    /// The node at `path`, if present.
    fn node(&self, path: &[&[u8]]) -> om_hbam::Result<Option<Cow<'_, OwnedNode>>>;

    /// The children of the node at `path`, `None` when there is no such
    /// node: a field's index (`[T, 01, F, kind]`) is read so. A source that
    /// can list a node's children a page at a time in key order gives them
    /// listed ([`Children::Listed`]), so that a program reads only the
    /// entries it asks for and never holds the index whole; by default the
    /// node is [`Source::node`]'s, whole.
    fn children(&self, path: &[&[u8]]) -> om_hbam::Result<Option<Children<'_>>> {
        Ok(self.node(path)?.map(Children::Whole))
    }

    /// Every record id of base table `table`: its record-id list `[T, 0d]`
    /// decoded (query-program.md 7.1), empty when the table has none. A
    /// source that can read the list's entries one at a time gives it
    /// without the list as a tree; by default it is [`Source::node`]'s
    /// node, decoded.
    fn record_ids(&self, table: &[u8]) -> om_hbam::Result<RidList> {
        match self.node(&[table, &[RECORD_IDS]])? {
            Some(node) => RidList::from_owned(&node),
            None => Ok(RidList::new()),
        }
    }

    /// The index `kind` (0 value, 1 word) of `field` in `table`, built from
    /// the stored records because a program reads it and the field has none
    /// (query-program.md 7.1: a host builds the index a finder reads on
    /// demand): a node of index keys, each holding its RID list. `None`
    /// when the source does not build indexes or the field cannot be
    /// indexed. [`crate::QueryError::UnkeyedIndex`] when the source cannot
    /// build the keys the file's index would hold (a text field in an index
    /// language whose keys it does not build): the program fails rather
    /// than read other keys.
    fn index_on_demand(
        &self,
        table: &[u8],
        field: &[u8],
        kind: u8,
    ) -> Result<Option<OwnedNode>, crate::QueryError> {
        let _ = (table, field, kind);
        Ok(None)
    }

    /// Whether a finder that reads a missing index gets no records, the
    /// index being built only for later programs, as FileMaker Server
    /// answers (query-program.md 7.1). `false`: [`Source::index_on_demand`]
    /// builds it first and the finder reads it. Also applies to
    /// ExistsInIndex, whose false answer needs the same presence check.
    fn finders_read_missing_indexes(&self) -> bool {
        false
    }

    /// GenerateMemberKey (schema-path.md 3.4): reserve the next member id
    /// and modification count of the catalog at `catalog`, returned as key
    /// strings (`[u8 length][serial key]`), so that no other client is given
    /// them. `None` when the source cannot reserve, as a file read on its
    /// own cannot.
    fn generate_member_key(&self, catalog: &[Vec<u8>]) -> Option<(Vec<u8>, Vec<u8>)> {
        let _ = catalog;
        None
    }
}

impl Source for HbamFile {
    fn catalog(&self) -> om_hbam::Result<Catalog> {
        HbamFile::catalog(self)
    }

    fn node(&self, path: &[&[u8]]) -> om_hbam::Result<Option<Cow<'_, OwnedNode>>> {
        Ok(self
            .subtree(path)?
            .map(|node| Cow::Owned(node.to_owned_node())))
    }
}
