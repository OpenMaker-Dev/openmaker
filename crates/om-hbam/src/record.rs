//! Record values: `<table>/5/<record>/<field>` (file-format.md 3.4, 3.6).
//!
//! Each stored field value is a child of the record node keyed by field id.
//! Observed values are masked text (numbers and dates in their stored text
//! form); long or non-ASCII text is a chunked value. Field ids not in the
//! table's field definitions are engine bookkeeping (e.g. `252`). Cells whose
//! structure is not a plain or chunked value (repetitions, containers - not
//! yet observed) are preserved as [`Cell::Nested`].

use std::collections::BTreeMap;

use crate::catalog::{keys, Table};
use crate::error::{Error, Result};
use crate::serial;
use crate::tree::{KeyTree, Node, OwnedNode};

/// One node below a nested cell: its path relative to the cell (segments as
/// raw bytes) and its value.
pub type NestedEntry = (Vec<Vec<u8>>, Option<Vec<u8>>);

/// One stored field value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    /// The stored bytes, reassembled if chunked (still XOR 0x5A masked).
    Value(Vec<u8>),
    /// A structure that is not a single value, every node below the cell.
    Nested(Vec<NestedEntry>),
}

impl Cell {
    /// The cell stored at a record's field node.
    pub fn from_node(node: &Node<'_>) -> Self {
        if let Some(v) = node.assembled() {
            return Cell::Value(v.into_owned());
        }
        let mut items = Vec::new();
        node.walk(&mut |path, n| {
            if !path.is_empty() {
                items.push((
                    path.iter().map(|s| s.0.to_vec()).collect(),
                    n.value.map(<[u8]>::to_vec),
                ));
            }
        });
        Cell::Nested(items)
    }

    /// The cell stored at a record's field node held in memory.
    pub fn from_owned(node: &OwnedNode) -> Self {
        if let Some(v) = node.assembled() {
            return Cell::Value(v.into_owned());
        }
        let mut items = Vec::new();
        fn walk(node: &OwnedNode, path: &mut Vec<Vec<u8>>, items: &mut Vec<NestedEntry>) {
            for child in &node.children {
                path.push(child.segment.clone());
                items.push((path.clone(), child.value.clone()));
                walk(child, path, items);
                path.pop();
            }
        }
        walk(node, &mut Vec::new(), &mut items);
        Cell::Nested(items)
    }

    /// The value as text (XOR 0x5A + SCSU); `None` for a nested cell.
    pub fn text(&self) -> Option<Result<String>> {
        match self {
            Cell::Value(v) => Some(om_scsu::decode_scsu(v).map_err(Error::from)),
            Cell::Nested(_) => None,
        }
    }

    /// Best-effort display string: text, else `0x`-prefixed hex of the stored
    /// bytes, else `<nested>`.
    pub fn display(&self) -> String {
        match (self, self.text()) {
            (_, Some(Ok(s))) => s,
            (Cell::Value(v), _) => format!(
                "0x{}",
                v.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ),
            (Cell::Nested(_), _) => "<nested>".to_string(),
        }
    }
}

/// One record: field id -> stored cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The record id. Record keys are serial keys
    /// (`docs/re/query-program.md` 2.6), the ids RID lists hold.
    pub id: u64,
    pub cells: BTreeMap<u64, Cell>,
}

impl Record {
    /// Text of a field, `None` when the record stores no value for it.
    pub fn text(&self, field_id: u64) -> Option<Result<String>> {
        self.cells.get(&field_id)?.text()
    }

    /// Every stored cell as a display string, keyed by field id.
    pub fn row(&self) -> BTreeMap<u64, String> {
        self.cells
            .iter()
            .map(|(id, c)| (*id, c.display()))
            .collect()
    }

    /// Values for `table`'s fields in field order (`None` = not stored).
    pub fn values_for(&self, table: &Table) -> Vec<Option<String>> {
        table
            .fields
            .iter()
            .map(|f| self.cells.get(&f.id).map(Cell::display))
            .collect()
    }
}

/// The records of `table_id` present in `tree`, in record-id key order.
pub fn records(tree: &KeyTree<'_>, table_id: u64) -> Vec<Record> {
    let Some(table) = tree.at(&[table_id, keys::TABLE_RECORDS]) else {
        return Vec::new();
    };
    table
        .children
        .iter()
        .filter_map(|rec| {
            let cells = rec
                .children
                .iter()
                .filter_map(|c| Some((c.id()?, Cell::from_node(c))))
                .collect();
            Some(Record {
                id: serial::decode(rec.segment.0)?,
                cells,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot::SlotReader;

    #[test]
    fn record_ids_are_serial_keys() {
        // Record 538 is keyed `81 9a`; its field 1 holds "Hi" (XOR 0x5A).
        let cell = OwnedNode::leaf(vec![0x01], vec![b'H' ^ 0x5a, b'i' ^ 0x5a]);
        let record = OwnedNode::list(vec![0x81, 0x9a], vec![cell]);
        let records = OwnedNode::list(vec![0x05], vec![record]);
        let table = OwnedNode::list(vec![0x80, 0x02], vec![records]);
        let wire = OwnedNode::list(Vec::new(), vec![table]).to_wire().unwrap();
        let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
        let records = super::records(&tree, 0x8002);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, 538);
        assert_eq!(records[0].text(1).unwrap().unwrap(), "Hi");
    }
}
