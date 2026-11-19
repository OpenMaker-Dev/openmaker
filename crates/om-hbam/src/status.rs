//! The file status area (`KStatus`, top key 2): file-intrinsic values the
//! engine reads when it opens a file.
//!
//! `mInternalCreation` is the u32 stored at `Concat(KStatus, KModified)` =
//! `[2, 254]` as a 4-byte big-endian leaf (no XOR 0x5A). It is minted when the
//! file is created and copied into files made from a template. Client and
//! server both read it from their copy of the file; it is the second half of
//! the op80 Login nonce (`docs/re/network-protocol.md` 4.3). A remote client
//! gets it from the first op90 Download after Open, whose reply is the
//! `KStatus` subtree as packed slots relative to base path `[2]` (see
//! [`status_tree_from_wire`]).

use crate::error::{Error, Result};
use crate::slot::{path_string, KeySegment, SlotReader};
use crate::tree::KeyTree;
use crate::HbamFile;

/// Key constants of the status area.
pub mod keys {
    /// `KStatus`: the file status area (top-level key).
    pub const STATUS: u64 = 2;
    /// `KModified`: under [`STATUS`], the `mInternalCreation` stamp.
    pub const MODIFIED: u64 = 254;
}

/// The numeric key path of `mInternalCreation`.
pub const INTERNAL_CREATION_PATH: [u64; 2] = [keys::STATUS, keys::MODIFIED];

/// The single-byte segment of [`keys::STATUS`], the base path of a status
/// subtree delivered on the wire.
const STATUS_SEGMENT: &[u8] = &[keys::STATUS as u8];

impl KeyTree<'_> {
    /// The file's `mInternalCreation`, or `None` when the tree has no
    /// `[2, 254]` key (e.g. a tree built from an unrelated subtree).
    ///
    /// Errors when the key holds anything other than a 4-byte value.
    pub fn internal_creation(&self) -> Result<Option<u32>> {
        let Some(node) = self.at(&INTERNAL_CREATION_PATH) else {
            return Ok(None);
        };
        let value = node.value.ok_or_else(|| Error::BadValue {
            path: path_string(&[KeySegment(STATUS_SEGMENT), node.segment]),
            reason: "mInternalCreation has no value",
        })?;
        let bytes: [u8; 4] = value.try_into().map_err(|_| Error::BadValue {
            path: path_string(&[KeySegment(STATUS_SEGMENT), node.segment]),
            reason: "mInternalCreation is not a 4-byte value",
        })?;
        Ok(Some(u32::from_be_bytes(bytes)))
    }
}

impl HbamFile {
    /// The file's `mInternalCreation` (see the module docs), read from the
    /// leaf chain. `None` if the file has no `[2, 254]` key.
    pub fn internal_creation(&self) -> Result<Option<u32>> {
        self.key_tree_under(&INTERNAL_CREATION_PATH)?
            .internal_creation()
    }
}

/// Build a key tree from a wire buffer holding the `KStatus` subtree as packed
/// slots relative to base path `[2]` (the slot bytes of the first op90
/// Download reply after Open, without the reply's trailer). Read
/// `mInternalCreation` from it with [`KeyTree::internal_creation`].
pub fn status_tree_from_wire(slots: &[u8]) -> Result<KeyTree<'_>> {
    KeyTree::from_slots(SlotReader::with_path(
        slots,
        vec![KeySegment(STATUS_SEGMENT)],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot::{encode_chunk, minimal_code};

    fn push(key: &[u8]) -> Vec<u8> {
        encode_chunk(minimal_code(true, key.len(), None).unwrap(), key, &[]).unwrap()
    }

    fn leaf(key: &[u8], value: &[u8]) -> Vec<u8> {
        encode_chunk(
            minimal_code(false, key.len(), Some(value.len())).unwrap(),
            key,
            value,
        )
        .unwrap()
    }

    /// The `KStatus` subtree as the wire carries it: leaves relative to `[2]`.
    fn status_children(modified: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend(leaf(&[3], &[0x6b, 0x68, 0x74, 0x6a]));
        buf.extend(leaf(&[5], &[0x04, 0xb8]));
        buf.extend(leaf(&[254], modified));
        buf
    }

    #[test]
    fn reads_internal_creation_from_a_root_based_buffer() {
        let mut buf = vec![0x80]; // open the root list
        buf.extend(push(&[2]));
        buf.push(0x80); // open 2's child list
        buf.extend(status_children(&[0xda, 0xdc, 0xff, 0xd3]));
        buf.extend([0x40, 0x40]); // close 2's list, then the root list
        let tree = KeyTree::from_slots(SlotReader::new(&buf)).unwrap();
        assert_eq!(tree.internal_creation().unwrap(), Some(0xdadc_ffd3));
    }

    #[test]
    fn reads_internal_creation_from_a_wire_status_subtree() {
        let mut buf = status_children(&[0xc0, 0x8c, 0x33, 0x93]);
        buf.push(0x40); // the wire subtree ends by closing [2]
        let tree = status_tree_from_wire(&buf).unwrap();
        assert_eq!(tree.internal_creation().unwrap(), Some(0xc08c_3393));
    }

    #[test]
    fn missing_key_is_none_and_bad_length_is_an_error() {
        let buf = leaf(&[3], &[1, 2, 3, 4]);
        let tree = status_tree_from_wire(&buf).unwrap();
        assert_eq!(tree.internal_creation().unwrap(), None);

        let buf = status_children(&[1, 2, 3]);
        let tree = status_tree_from_wire(&buf).unwrap();
        assert!(matches!(
            tree.internal_creation(),
            Err(Error::BadValue { .. })
        ));
    }
}
