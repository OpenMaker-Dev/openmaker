//! Modification counts (`fc`, `KModCount`): a serial key with its length
//! byte, `01 05` = 5, absent = 0 (schema-path.md 8.1, write-path.md 5.4).
//! Records, catalogs and library members carry one; clients compare them
//! to spot stale copies. A record's count is the committing client's to
//! keep, so the server checks the form of one it is asked to store
//! ([`counted`]) and reads anything else as 0.

use om_hbam::{serial, OwnedNode};

/// The key of a modification count.
pub const MOD_COUNT: u8 = 0xfc;

/// The count `value` holds in FileMaker's form: a length byte, then a
/// serial key of that length (2 to 10 bytes in all). `None` otherwise.
pub fn parse(value: &[u8]) -> Option<u64> {
    let (&len, key) = value.split_first()?;
    if usize::from(len) != key.len() {
        return None;
    }
    serial::decode(key)
}

/// Whether `node`, an `fc` key, is a count: a leaf whose value [`parse`]s.
pub fn is_count(node: &OwnedNode) -> bool {
    !node.push && node.children.is_empty() && node.value.as_deref().and_then(parse).is_some()
}

/// Whether `node` (a record) has no modification count or one that
/// [`is_count`]: what a commit may store.
pub fn counted(node: &OwnedNode) -> bool {
    node.child(&[MOD_COUNT]).map_or(true, is_count)
}

/// A `[u8 length][serial key]` value, 0 when absent or malformed.
pub fn serial_value(node: Option<&OwnedNode>) -> u64 {
    node.and_then(|n| n.value.as_deref())
        .and_then(parse)
        .unwrap_or(0)
}

/// `value` as a key string, `[u8 length][serial key]`.
pub fn key_string(value: u64) -> Vec<u8> {
    let segment = serial::encode(value);
    let mut bytes = vec![segment.len() as u8];
    bytes.extend(segment);
    bytes
}

/// A leaf `key` holding `value` as a key string.
pub fn serial_leaf(key: u8, value: u64) -> OwnedNode {
    OwnedNode::leaf(vec![key], key_string(value))
}

/// Add `by` to the modification count of `node` and return the new count.
/// `None`, leaving `node` as it is, when the count would pass the largest a
/// serial key holds: a count comes from the file or a client (op70 may store
/// any count), and one that wrapped around or stayed put would hide the
/// change from clients that compare counts to spot stale copies, so the
/// caller must not make the change (#361).
#[must_use]
pub fn bump(node: &mut OwnedNode, by: u64) -> Option<u64> {
    let count = serial_value(node.child(&[MOD_COUNT])).checked_add(by)?;
    node.set_child(serial_leaf(MOD_COUNT, count));
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_serial_keys() {
        let mut node = OwnedNode::default();
        assert_eq!(bump(&mut node, 1), Some(1));
        assert_eq!(
            node.child(&[MOD_COUNT]).unwrap().value.as_deref(),
            Some(&[1, 1][..])
        );
        assert_eq!(bump(&mut node, 127), Some(128));
        assert_eq!(
            node.child(&[MOD_COUNT]).unwrap().value.as_deref(),
            Some(&[2, 0x80, 0x00][..])
        );
        assert_eq!(serial_value(node.child(&[MOD_COUNT])), 128);
        // The largest count does not advance, and stays as it was (#361).
        node.set_child(serial_leaf(MOD_COUNT, u64::MAX - 1));
        assert_eq!(bump(&mut node, 1), Some(u64::MAX));
        let largest = node.clone();
        assert_eq!(bump(&mut node, 1), None);
        assert_eq!(node, largest);
    }
}
