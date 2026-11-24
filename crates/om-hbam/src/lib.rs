//! FileMaker HBAM7 (`.fmp12`) container and packed-slot reader.
//!
//! Spec: `docs/re/file-format.md` (container, pages, key space, chunk
//! encoding). The chunk stream of a page is the same packed-slot buffer that
//! record transactions carry on the wire (`docs/re/network-protocol.md` 5.5),
//! so one decoder serves local files and server responses:
//!
//! ```text
//! file:  HbamFile -> leaf chain -> SlotReader per page -> KeyTree
//! wire:  packed slot buffer     -> SlotReader          -> KeyTree
//!        KeyTree -> Catalog (tables, fields, occurrences, layouts) / records
//! ```
//!
//! Parsed: superblock magic and page size, page headers, the leaf chain (with
//! link validation), every chunk (sizes, hierarchy), key order, chunked
//! values, text (XOR 0x5A + SCSU), table/field/occurrence/layout names, field
//! kind and type, partial read-only field auto-entry/validation metadata,
//! layout protobuf blobs (raw), record values, and the
//! file's `mInternalCreation` stamp (the Login nonce input, [`status`]).
//!
//! Preserved opaque: the superblock beyond magic/page size/signature (version
//! record, counter, identity fence), index pages, page slack, field option
//! bits not explicitly decoded, occurrence definitions, layout protobufs (unparsed), and
//! every key not decoded into a typed struct (reachable via [`KeyTree`]).
//! Nothing is copied or rewritten, so [`HbamFile::as_bytes`] round-trips.
//!
//! Which paths of a tree are Design, Data or Access, the file's design
//! objects and their revisions: [`design_paths`] (`docs/server-storage.md`
//! sections 4 and 9).

pub mod catalog;
pub mod container;
pub mod design_paths;
pub mod error;
pub mod field_options;
pub mod record;
pub mod ridlist;
pub mod serial;
pub mod slot;
pub mod status;
pub mod tree;
pub mod writer;

pub use catalog::{
    Catalog, DataType, Field, FieldKind, GraphBox, Layout, LayoutPart, Table, TableOccurrence,
};
pub use container::{new_superblock, HbamFile, Page, PageHeader, Superblock, MAX_CREATION_NAME};
pub use error::{Error, Result};
pub use field_options::{EntryVariable, FieldOptions, SimpleGetSelector};
pub use record::{Cell, Record};
pub use ridlist::RidList;
pub use slot::{Chunk, ChunkKind, KeySegment, Slot, SlotReader};
pub use status::{status_tree_from_wire, INTERNAL_CREATION_PATH};
pub use tree::{KeyTree, Node, OwnedNode};

impl HbamFile {
    /// Decode the schema catalogs (tables, fields, occurrences, layouts),
    /// skipping record data.
    pub fn catalog(&self) -> Result<Catalog> {
        Catalog::from_tree(&KeyTree::from_slots_where(
            self.slots(),
            catalog::is_catalog_path,
        )?)
    }

    /// All records of base table `table_id`.
    pub fn records(&self, table_id: u64) -> Result<Vec<Record>> {
        let tree = self.key_tree_under(&[table_id, catalog::keys::TABLE_RECORDS])?;
        Ok(record::records(&tree, table_id))
    }

    /// One record's stored value for a field, `None` when the record stores
    /// none.
    pub fn cell(&self, table_id: u64, record_id: u64, field_id: u64) -> Result<Option<Cell>> {
        let path = [
            slot::uint_segment(table_id),
            slot::uint_segment(catalog::keys::TABLE_RECORDS),
            serial::encode(record_id),
            slot::uint_segment(field_id),
        ];
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        Ok(self.subtree(&segments)?.map(|node| Cell::from_node(&node)))
    }
}
