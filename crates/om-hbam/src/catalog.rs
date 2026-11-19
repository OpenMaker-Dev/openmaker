//! Typed schema catalogs: base tables and their fields, table occurrences,
//! and layouts.
//!
//! Key map (file-format.md 3.4, refined in 3.6 from the shipped templates and
//! the FileMaker Server sample file):
//!
//! | path | holds |
//! |------|-------|
//! | `3/16/5/<table>` | base table; `16` = name. `<table>` (>= 0x8000) is also the table's top-level data key |
//! | `3/17/5/<occurrence>` | table occurrence; `16` = name, `2` = definition (opaque) |
//! | `<table>/3/5/<field>` | field; `16` = name, `2` = 26-byte options (byte 0 kind, byte 1 type) |
//! | `<table>/5/<record>/<field>` | record values (see [`crate::record`]) |
//! | `4/5/<layout>` | layout; `16` = name, `2` = `LayoutDef`, `3/<part>/2` = `PartDef`, `7` = `LayoutObjectStore` |
//!
//! Everything not decoded into a typed field stays reachable through the
//! [`KeyTree`] (and so round-trips byte for byte).

use crate::error::Result;
use crate::tree::{KeyTree, Node};

/// Top-level and per-catalog key numbers used here.
pub mod keys {
    /// Schema catalog: base tables and table occurrences.
    pub const SCHEMA: u64 = 3;
    /// Under [`SCHEMA`]: base tables.
    pub const BASE_TABLES: u64 = 16;
    /// Under [`SCHEMA`]: table occurrences (relationship graph nodes).
    pub const TABLE_OCCURRENCES: u64 = 17;
    /// Layout catalog (`KLayoutDefs`).
    pub const LAYOUTS: u64 = 4;
    /// Within a catalog: the name/id index (collated name -> id).
    pub const NAME_INDEX: u64 = 1;
    /// Within a catalog: the entry definitions, keyed by entry id.
    pub const ENTRIES: u64 = 5;
    /// Within an entry: its name (masked text).
    pub const NAME: u64 = 16;
    /// Within an entry: its main definition blob (field options, `LayoutDef`).
    pub const DEFINITION: u64 = 2;
    /// Under a table's data key: field definitions (`<table>/3/5/<field>`).
    pub const TABLE_FIELDS: u64 = 3;
    /// Under a table's data key: records (`<table>/5/<record>/<field>`).
    pub const TABLE_RECORDS: u64 = 5;
    /// Within a layout: parts (`3/<part>/2` = `PartDef`).
    pub const LAYOUT_PARTS: u64 = 3;
    /// Within a layout: the `LayoutObjectStore` protobuf, usually chunked.
    pub const LAYOUT_OBJECTS: u64 = 7;
}

/// Field kind, options byte 0 [I: from field names across the samples].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// Stored data entry field.
    Normal,
    Calculation,
    Summary,
    Unknown(u8),
}

/// Field data (or calculation result) type, options byte 1 [I].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataType {
    Text,
    Number,
    Date,
    Time,
    Timestamp,
    Container,
    Unknown(u8),
}

impl FieldKind {
    fn from_byte(b: u8) -> Self {
        match b {
            1 => FieldKind::Normal,
            2 => FieldKind::Calculation,
            3 => FieldKind::Summary,
            other => FieldKind::Unknown(other),
        }
    }
}

impl DataType {
    fn from_byte(b: u8) -> Self {
        match b {
            1 => DataType::Text,
            2 => DataType::Number,
            3 => DataType::Date,
            4 => DataType::Time,
            5 => DataType::Timestamp,
            6 => DataType::Container,
            other => DataType::Unknown(other),
        }
    }
}

/// A field definition (`<table>/3/5/<field>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub id: u64,
    pub name: String,
    pub kind: FieldKind,
    pub data_type: DataType,
    /// The raw options blob (`.../2`). `decoded_options` interprets supported
    /// fixed-size metadata without changing or evaluating it.
    pub options: Vec<u8>,
}

impl Field {
    pub fn decoded_options(&self) -> Option<crate::FieldOptions> {
        crate::FieldOptions::decode(&self.options)
    }
}

/// A base table (`3/16/5/<id>`) with its fields in id order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// Table id; also the top-level key of its fields and records.
    pub id: u64,
    pub name: String,
    pub fields: Vec<Field>,
}

impl Table {
    pub fn field(&self, id: u64) -> Option<&Field> {
        self.fields.iter().find(|f| f.id == id)
    }

    pub fn field_by_name(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// A table occurrence (`3/17/5/<id>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableOccurrence {
    pub id: u64,
    pub name: String,
    /// The raw definition blob (`.../2`, opaque; it references the base table).
    pub definition: Vec<u8>,
}

impl TableOccurrence {
    /// The data source and the base table this occurrence shows. The
    /// definition starts with two key strings `[u8 total][u8 len][segment]`:
    /// the data source (`02 01 00`, 0, for this file; else a member of
    /// `[32, 5]`), then the base table's key in that source's file, e.g.
    /// `03 02 80 02` for table `80 02`; more bytes follow (file-metadata.md
    /// 1.1). Both ids are their segment's big-endian reading, as the catalog
    /// paths number members, so a data source past 127 (`03 02 80 00`)
    /// keeps both bytes. `None` for any other form.
    pub fn reference(&self) -> Option<(u64, u64)> {
        let mut rest = self.definition.as_slice();
        let source = key_string(&mut rest)?;
        let table = key_string(&mut rest)?;
        Some((source, table))
    }

    /// The id of the base table this occurrence shows, when it is a table
    /// of this file (data source 0, [`Self::reference`]).
    pub fn base_table(&self) -> Option<u64> {
        let (source, table) = self.reference()?;
        (source == 0).then_some(table)
    }

    /// Where the relationships graph draws this occurrence: the 28 bytes
    /// after the two key strings of [`Self::reference`] (schema-path.md
    /// 5.3). `None` unless exactly 28 bytes follow and the four
    /// coordinates are finite.
    pub fn graph_box(&self) -> Option<GraphBox> {
        let mut rest = self.definition.as_slice();
        key_string(&mut rest)?;
        key_string(&mut rest)?;
        let bytes: &[u8; 28] = rest.try_into().ok()?;
        let f32_at = |at: usize| {
            let value =
                f32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
            value.is_finite().then_some(value)
        };
        let u32_at = |at: usize| {
            u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        Some(GraphBox {
            x: f32_at(0)?,
            y: f32_at(4)?,
            width: f32_at(8)?,
            height: f32_at(12)?,
            list_height: u32_at(16),
            display: u32_at(20),
            color: [bytes[24], bytes[25], bytes[26], bytes[27]],
        })
    }
}

/// An occurrence's box in the relationships graph (schema-path.md 5.3), in
/// points from the graph's top left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    /// The height with every field shown.
    pub height: f32,
    /// The u32 after the box: `height` less the title bar and key-field
    /// area in every sample, so the field list's height [I].
    pub list_height: u32,
    /// The u32 after that: 0 shows every field, 1 only the key fields [O
    /// for 0 and 1]; other values are not observed.
    pub display: u32,
    /// The box colour, RGBA.
    pub color: [u8; 4],
}

/// The one-segment key string at the front of `bytes`, read big-endian;
/// `bytes` then starts after it. `None` unless the segment fills the key.
fn key_string(bytes: &mut &[u8]) -> Option<u64> {
    let (&total, rest) = bytes.split_first()?;
    let key = rest.get(..usize::from(total))?;
    let (&len, segment) = key.split_first()?;
    if usize::from(len) != segment.len() {
        return None;
    }
    let id = crate::slot::be_uint(segment)?;
    *bytes = &rest[key.len()..];
    Some(id)
}

/// A layout part (`4/5/<layout>/3/<part>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutPart {
    pub id: u64,
    /// `FMLayoutPB.PartDef` protobuf, raw.
    pub part_def: Vec<u8>,
}

/// A layout (`4/5/<id>`). Protobuf blobs are raw (not XOR masked) and kept
/// unparsed so they round-trip with unknown fields intact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub id: u64,
    pub name: String,
    /// `FMLayoutPB.LayoutDef` protobuf (`.../2`).
    pub layout_def: Option<Vec<u8>>,
    /// `FMLayoutPB.LayoutObjectStore` protobuf (`.../7`), reassembled.
    pub object_store: Option<Vec<u8>>,
    pub parts: Vec<LayoutPart>,
}

/// The schema of a file: tables, occurrences, and layouts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    pub tables: Vec<Table>,
    pub table_occurrences: Vec<TableOccurrence>,
    pub layouts: Vec<Layout>,
}

impl Catalog {
    /// Decode every catalog present in `tree`. A tree built from a partial
    /// source (a wire buffer) yields only the catalogs it contains.
    pub fn from_tree(tree: &KeyTree<'_>) -> Result<Self> {
        Ok(Catalog {
            tables: tables(tree)?,
            table_occurrences: table_occurrences(tree)?,
            layouts: layouts(tree)?,
        })
    }

    pub fn table(&self, id: u64) -> Option<&Table> {
        self.tables.iter().find(|t| t.id == id)
    }

    pub fn table_by_name(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
    }

    pub fn layout_by_name(&self, name: &str) -> Option<&Layout> {
        self.layouts.iter().find(|l| l.name == name)
    }
}

/// Whether a path belongs to the schema catalogs [`Catalog::from_tree`] reads
/// (so a file reader can skip record data).
pub fn is_catalog_path(path: &[crate::slot::KeySegment<'_>]) -> bool {
    match (
        path.first().and_then(|s| s.as_uint()),
        path.get(1).and_then(|s| s.as_uint()),
    ) {
        (Some(keys::SCHEMA | keys::LAYOUTS), _) => true,
        (Some(t), second) if t >= 0x8000 => second.map_or(true, |s| s == keys::TABLE_FIELDS),
        _ => false,
    }
}

fn entries<'t, 'a>(
    tree: &'t KeyTree<'a>,
    catalog: &[u64],
) -> impl Iterator<Item = (u64, &'t Node<'a>)> {
    let path: Vec<u64> = catalog.iter().copied().chain([keys::ENTRIES]).collect();
    tree.at(&path)
        .map(|n| n.children.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|n| Some((n.id()?, n)))
}

fn name_of(entry: &Node<'_>) -> Result<String> {
    entry
        .get(keys::NAME)
        .and_then(Node::text)
        .transpose()
        .map(Option::unwrap_or_default)
}

fn blob(entry: &Node<'_>, id: u64) -> Option<Vec<u8>> {
    entry.get(id)?.assembled().map(|v| v.into_owned())
}

/// Base tables with their fields (fields need the `<table>/3` subtree).
pub fn tables(tree: &KeyTree<'_>) -> Result<Vec<Table>> {
    entries(tree, &[keys::SCHEMA, keys::BASE_TABLES])
        .map(|(id, entry)| {
            Ok(Table {
                id,
                name: name_of(entry)?,
                fields: fields(tree, id)?,
            })
        })
        .collect()
}

/// The field definitions of base table `table_id`.
pub fn fields(tree: &KeyTree<'_>, table_id: u64) -> Result<Vec<Field>> {
    entries(tree, &[table_id, keys::TABLE_FIELDS])
        .map(|(id, entry)| {
            let options = blob(entry, keys::DEFINITION).unwrap_or_default();
            let byte = |i: usize| options.get(i).copied().unwrap_or(0);
            Ok(Field {
                id,
                name: name_of(entry)?,
                kind: FieldKind::from_byte(byte(0)),
                data_type: DataType::from_byte(byte(1)),
                options,
            })
        })
        .collect()
}

pub fn table_occurrences(tree: &KeyTree<'_>) -> Result<Vec<TableOccurrence>> {
    entries(tree, &[keys::SCHEMA, keys::TABLE_OCCURRENCES])
        .map(|(id, entry)| {
            Ok(TableOccurrence {
                id,
                name: name_of(entry)?,
                definition: blob(entry, keys::DEFINITION).unwrap_or_default(),
            })
        })
        .collect()
}

/// Layouts in id order.
pub fn layouts(tree: &KeyTree<'_>) -> Result<Vec<Layout>> {
    entries(tree, &[keys::LAYOUTS])
        .map(|(id, entry)| {
            let parts = entry
                .get(keys::LAYOUT_PARTS)
                .map(|p| p.children.as_slice())
                .unwrap_or_default()
                .iter()
                .filter_map(|part| {
                    Some(LayoutPart {
                        id: part.id()?,
                        part_def: blob(part, keys::DEFINITION)?,
                    })
                })
                .collect();
            Ok(Layout {
                id,
                name: name_of(entry)?,
                layout_def: blob(entry, keys::DEFINITION),
                object_store: blob(entry, keys::LAYOUT_OBJECTS),
                parts,
            })
        })
        .collect()
}

/// The layouts the layout catalog's index `[4, 1]` lists, by its member
/// entries `[4, 1, 07, L]`: each one's id, name and definition (`02`, its
/// table occurrence and menu set, layout-model.md 10.2), with no object
/// store or parts. What a client reads before a layout's member `[4, 5, L]`
/// (FileMaker Pro 26 reads the index, then the members it shows, #1018).
pub fn layout_index(tree: &KeyTree<'_>) -> Result<Vec<Layout>> {
    let path = [keys::LAYOUTS, INDEX, INDEX_MEMBERS];
    tree.at(&path)
        .map(|n| n.children.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|n| Some((n.id()?, n)))
        .map(|(id, entry)| {
            Ok(Layout {
                id,
                name: name_of(entry)?,
                layout_def: blob(entry, keys::DEFINITION),
                object_store: None,
                parts: Vec::new(),
            })
        })
        .collect()
}

/// A catalog's index `[C, 1]` and its member entries `[C, 1, 07]`.
const INDEX: u64 = 1;
const INDEX_MEMBERS: u64 = 7;

#[cfg(test)]
mod tests {
    use super::*;

    fn occurrence(definition: &[u8]) -> TableOccurrence {
        TableOccurrence {
            id: 0xd0_0002,
            name: "Contacts".into(),
            definition: definition.to_vec(),
        }
    }

    #[test]
    fn occurrence_names_its_base_table() {
        assert_eq!(
            occurrence(&[2, 1, 0, 3, 2, 0x80, 2]).base_table(),
            Some(0x8002)
        );
        // Definitions carry more bytes after the key string.
        assert_eq!(
            occurrence(&[2, 1, 0, 3, 2, 0x80, 4, 0, 0, 1]).base_table(),
            Some(0x8004)
        );
        for unknown in [
            &[1, 1, 0, 3, 2, 0x80, 2][..],
            &[2, 1, 0, 3, 4, 0x80, 2],
            &[2, 1, 0, 4, 2, 0x80, 2],
            &[2, 1, 0],
            &[],
        ] {
            assert_eq!(occurrence(unknown).base_table(), None, "{unknown:02x?}");
        }
        // Another file's table is not a base table of this file.
        assert_eq!(occurrence(&[2, 1, 1, 2, 1, 0x81]).base_table(), None);
    }

    /// The key string of a catalog member with serial id `id`.
    fn member_key(id: u64) -> Vec<u8> {
        let segment = crate::serial::encode(id);
        let mut key = vec![segment.len() as u8 + 1, segment.len() as u8];
        key.extend(segment);
        key
    }

    #[test]
    fn occurrences_name_their_data_source_and_its_table_by_whole_keys() {
        for source in [0, 1, 127, 128, 0x1234] {
            for table in [0x81, 130, 0x10_0000] {
                let mut definition = member_key(source);
                definition.extend(member_key(table));
                definition.extend([0, 0, 1]);
                let raw = |id| crate::serial::raw_of_id(id).unwrap();
                assert_eq!(
                    occurrence(&definition).reference(),
                    Some((raw(source), raw(table))),
                    "{definition:02x?}"
                );
            }
        }
        // Data source 128 is `80 00`, not a one-byte id `80`.
        assert_eq!(
            occurrence(&[3, 2, 0x80, 0x00, 2, 1, 0x81]).reference(),
            Some((0x8000, 0x81))
        );
        for malformed in [
            &[3, 2, 0x80, 2, 1, 0x81][..],
            &[3, 3, 0x80, 0x00, 2, 1, 0x81],
            &[2, 1, 1, 2, 2, 0x81],
            &[2, 1, 1, 3, 2, 0x81],
            &[2, 1, 1],
            &[1, 0, 2, 1, 0x81],
            &[0],
        ] {
            assert_eq!(occurrence(malformed).reference(), None, "{malformed:02x?}");
        }
    }

    fn hex(digits: &str) -> Vec<u8> {
        let digits: String = digits.split_whitespace().collect();
        (0..digits.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The Contacts starter solution's Contacts and Addresses occurrences,
    /// as FileMaker Pro 26 ships them, which it draws at their stored
    /// boxes, Contacts with every field and Addresses with its key field
    /// only (schema-path.md 5.3).
    #[test]
    fn occurrence_graph_boxes_decode_from_the_bytes_after_the_keys() {
        let contacts =
            hex("020100 03028002 41a00000 41a00000 43030000 43620000 000000b6 00000000 787878ff");
        assert_eq!(
            occurrence(&contacts).graph_box(),
            Some(GraphBox {
                x: 20.0,
                y: 20.0,
                width: 131.0,
                height: 226.0,
                list_height: 182,
                display: 0,
                color: [0x78, 0x78, 0x78, 0xff],
            })
        );
        let addresses =
            hex("020100 03028003 43520000 41a00000 43030000 42ec0000 0000004a 00000001 787878ff");
        let graph = occurrence(&addresses).graph_box().unwrap();
        assert_eq!((graph.x, graph.height, graph.display), (210.0, 118.0, 1));
        // An occurrence of another file's table: a longer data source key.
        let mut external = vec![3, 2, 0x80, 0x00];
        external.extend(&contacts[3..]);
        assert_eq!(
            occurrence(&external).graph_box(),
            occurrence(&contacts).graph_box()
        );
        // A short or long tail, a NaN or infinite coordinate, a broken key.
        let nan = [&contacts[..7], &[0x7f, 0xc0, 0, 0], &contacts[11..]].concat();
        let infinite = [&contacts[..19], &[0x7f, 0x80, 0, 0], &contacts[23..]].concat();
        for malformed in [
            &contacts[..34],
            &[&contacts[..], &[0]].concat()[..],
            &nan[..],
            &infinite[..],
            &contacts[..7],
            &[&[2, 1, 0, 3, 3, 0x80, 2], &contacts[7..]].concat()[..],
        ] {
            assert_eq!(occurrence(malformed).graph_box(), None, "{malformed:02x?}");
        }
    }
}
