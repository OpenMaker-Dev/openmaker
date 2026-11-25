//! What a program reads: a file on disk, or a key tree a host keeps in
//! memory and changes as clients write.

use std::borrow::Cow;

use om_hbam::{Catalog, HbamFile, OwnedNode, RidList};

/// Key of a table's record-id list.
const RECORD_IDS: u8 = 13;

/// The data a program runs against.
pub trait Source {
    /// The file's schema (tables and fields).
    fn catalog(&self) -> om_hbam::Result<Catalog>;

    /// The node at `path`, if present.
    fn node(&self, path: &[&[u8]]) -> om_hbam::Result<Option<Cow<'_, OwnedNode>>>;

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
