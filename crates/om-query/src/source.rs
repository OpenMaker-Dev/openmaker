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

/// A node's children listed a page at a time by key, in key order, from a
/// bound (`[T, 01, F, kind]`'s entries: query-program.md 7.1), each read
/// with what is below it only when asked for by its key, so that a reader
/// holds only the entries it takes: a lookup that misses, and the keys a
/// scan passes, read no entry's records.
pub trait Listing {
    /// Up to `max` children's keys onto `out`, nothing below them: forward,
    /// those at or after an included `from` (after an excluded one, from
    /// the first when unbounded), in strictly increasing order; backward,
    /// those at or before it (before an excluded one, from the last when
    /// unbounded), in strictly decreasing order. Whether more may follow
    /// the last one given: a listing may give fewer than `max` and still
    /// have more, but gives at least one while any is left.
    fn keys<'a>(
        &'a self,
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Cow<'a, [u8]>>,
    ) -> om_hbam::Result<bool>;

    /// The child under `key` and everything below it (an index entry and
    /// its RID list), `None` when there is none.
    fn entry<'a>(&'a self, key: &[u8]) -> om_hbam::Result<Option<Cow<'a, OwnedNode>>>;
}

/// A listing that gave a key out of the order every listing keeps (or
/// outside the bound it was asked from): the source is broken, and its
/// reader fails rather than read on.
pub(crate) fn out_of_order() -> om_hbam::Error {
    om_hbam::Error::KeyOrder {
        path: "an index's listing".into(),
    }
}

/// A listing that broke its contract otherwise ([`Listing`]): a page with
/// no key that says more follow, which would end a read early unnoticed,
/// or a key listed without an entry.
pub(crate) fn broken(reason: &'static str) -> om_hbam::Error {
    om_hbam::Error::BadValue {
        path: "an index's listing".into(),
        reason,
    }
}

/// A listing of a node held whole that gives at most `cap` keys a page,
/// so that a test's scans cross pages.
#[cfg(test)]
pub(crate) struct Capped<'n> {
    pub node: &'n OwnedNode,
    pub cap: usize,
}

#[cfg(test)]
impl Listing for Capped<'_> {
    fn keys<'a>(
        &'a self,
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Cow<'a, [u8]>>,
    ) -> om_hbam::Result<bool> {
        let mut page = Vec::new();
        let more = node_keys(self.node, from, direction, self.cap, &mut page);
        let more = more || page.len() > max;
        out.extend(page.into_iter().take(max));
        Ok(more)
    }

    fn entry<'a>(&'a self, key: &[u8]) -> om_hbam::Result<Option<Cow<'a, OwnedNode>>> {
        Ok(node_entry(self.node, key))
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
    fn keys<'a>(
        &'a self,
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Cow<'a, [u8]>>,
    ) -> om_hbam::Result<bool> {
        Ok(node_keys(self.0, from, direction, max, out))
    }

    fn entry<'a>(&'a self, key: &[u8]) -> om_hbam::Result<Option<Cow<'a, OwnedNode>>> {
        Ok(node_entry(self.0, key))
    }
}

/// [`Listing::keys`] over the children of `node`, which are in order.
pub(crate) fn node_keys<'n>(
    node: &'n OwnedNode,
    from: Bound<&[u8]>,
    direction: Direction,
    max: usize,
    out: &mut Vec<Cow<'n, [u8]>>,
) -> bool {
    let children = &node.children;
    let at = |key: &[u8], after: bool| {
        children.partition_point(|c| match after {
            true => c.segment.as_slice() <= key,
            false => c.segment.as_slice() < key,
        })
    };
    let key = |c: &'n OwnedNode| Cow::Borrowed(c.segment.as_slice());
    match direction {
        Direction::Forward => {
            let start = match from {
                Bound::Included(k) => at(k, false),
                Bound::Excluded(k) => at(k, true),
                Bound::Unbounded => 0,
            };
            let rest = &children[start..];
            out.extend(rest.iter().take(max).map(key));
            rest.len() > max
        }
        Direction::Backward => {
            let end = match from {
                Bound::Included(k) => at(k, true),
                Bound::Excluded(k) => at(k, false),
                Bound::Unbounded => children.len(),
            };
            let rest = &children[..end];
            out.extend(rest.iter().rev().take(max).map(key));
            rest.len() > max
        }
    }
}

/// [`Listing::entry`] over the children of `node`, which are in order.
pub(crate) fn node_entry<'n>(node: &'n OwnedNode, key: &[u8]) -> Option<Cow<'n, OwnedNode>> {
    node.children
        .binary_search_by(|c| c.segment.as_slice().cmp(key))
        .ok()
        .map(|at| Cow::Borrowed(&node.children[at]))
}

/// A node without its children: whether it has any (or is a list without
/// them, `push`), and its own value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub list: bool,
    pub value: Option<Vec<u8>>,
}

impl Head {
    /// The head of `node`.
    pub fn of(node: &OwnedNode) -> Self {
        Self {
            list: node.push || !node.children.is_empty(),
            value: node.value.clone(),
        }
    }
}

/// The data a program runs against.
pub trait Source {
    /// The file's schema (tables and fields).
    fn catalog(&self) -> om_hbam::Result<Catalog>;

    /// The node at `path`, if present.
    fn node(&self, path: &[&[u8]]) -> om_hbam::Result<Option<Cow<'_, OwnedNode>>>;

    /// The node at `path` without its children, `None` when there is no
    /// such node: what NodeExists and GetKey read (spec 8.5), which a
    /// source that can read a node alone answers without what is below it
    /// (a field's index, under a path a client names). By default
    /// [`Source::node`]'s, whole.
    fn head(&self, path: &[&[u8]]) -> om_hbam::Result<Option<Head>> {
        Ok(self.node(path)?.map(|node| Head::of(&node)))
    }

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
