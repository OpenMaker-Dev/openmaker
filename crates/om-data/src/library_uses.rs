//! The file library's use counts (docs/re/layout-model.md 10.8, 10.10):
//! each library item `[31, 5, N]` counts the objects that show it at `12`.
//! When objects that show items go (Delete Layout), their uses are
//! released: each item's count is lowered by its released uses and its
//! stamp count `fc` raised by as many, as FileMaker Pro's own deletion does;
//! an item left with no use is removed, with its entries in the library's
//! index `[31, 1]` (`06, N`, `07, N` and `08, <the item's key 04>, N`) and
//! the index's count `04` lowered. Every other key is kept as stored.

use std::collections::BTreeMap;

use om_hbam::{serial, OwnedNode};

/// Keys of the library catalog `[31]` and of its index and members.
const INDEX: u8 = 0x01;
const MEMBERS: u8 = 0x05;
const INDEX_COUNT: u8 = 0x04;
const INDEX_06: u8 = 0x06;
const INDEX_07: u8 = 0x07;
const INDEX_BY_KEY: u8 = 0x08;
const MEMBER_KEY: u8 = 0x04;
const USES: u8 = 0x12;
const MOD_COUNT: [u8; 1] = [0xfc];

/// The library after uses were released.
#[derive(Clone, Debug, PartialEq)]
pub struct Released {
    /// Each changed item by its key segment: its new member, or `None`
    /// when it is removed.
    pub members: Vec<(Vec<u8>, Option<OwnedNode>)>,
    /// The library's index `[31, 1]` when an item was removed, without its
    /// stamp (the host stamps it).
    pub index: Option<OwnedNode>,
    /// What changed, as the layout plans list it.
    pub changes: Vec<(String, String)>,
}

/// Why uses cannot be released; nothing is changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A stamp count `fc` (a key string) raised by `by`.
fn raised_count(member: &OwnedNode, by: u32) -> Option<Vec<u8>> {
    let count = match member.child(&MOD_COUNT) {
        Some(node) => {
            let value = node.value.as_deref()?;
            let (&len, key) = value.split_first()?;
            if usize::from(len) != key.len() {
                return None;
            }
            serial::decode(key)?
        }
        None => 0,
    };
    let key = serial::encode(count.checked_add(u64::from(by))?);
    let mut out = vec![key.len() as u8];
    out.extend(key);
    Some(out)
}

/// Release `uses` (item id to the number of uses that go) from `library`,
/// the library catalog `[31]` as stored. An item that is missing, whose
/// count does not read, or that counts fewer uses than released is
/// refused: OpenMaker does not guess at a count FileMaker keeps.
pub fn release(library: &OwnedNode, uses: &BTreeMap<u64, u32>) -> Result<Released, Refusal> {
    let mut members = Vec::with_capacity(uses.len());
    let mut changes = Vec::new();
    let mut index = library.child(&[INDEX]).cloned();
    let mut removed = 0u32;
    for (&item, &released) in uses {
        let key = serial::encode(item);
        let unreadable = || {
            Refusal(format!(
                "Library item {item}'s use count is not one OpenMaker reads; nothing was \
                 written."
            ))
        };
        let member = library
            .at(&[&[MEMBERS], &key])
            .ok_or_else(|| Refusal(format!("The file library has no item {item}.")))?;
        let count = member
            .child(&[USES])
            .and_then(|n| n.value.as_deref())
            .and_then(|v| <[u8; 4]>::try_from(v).ok())
            .map(u32::from_be_bytes)
            .ok_or_else(unreadable)?;
        let left = count.checked_sub(released).ok_or_else(|| {
            Refusal(format!(
                "Library item {item} counts {count} uses, fewer than the {released} that go; \
                 nothing was written."
            ))
        })?;
        if left > 0 {
            let mut member = member.clone();
            member.set_child(OwnedNode::leaf(vec![USES], left.to_be_bytes().to_vec()));
            member.set_child(OwnedNode::leaf(
                MOD_COUNT.to_vec(),
                raised_count(&member, released).ok_or_else(unreadable)?,
            ));
            changes.push((
                format!("[1f, 05, {}, 12]", hex(&key)),
                format!("library item {item}'s use count {count} to {left}"),
            ));
            members.push((key, Some(member)));
            continue;
        }
        // No use left: the item goes, with its index entries.
        let index = index.as_mut().ok_or_else(|| {
            Refusal("The file library has no index [31, 1]; nothing was written.".into())
        })?;
        let by_key = member
            .child(&[MEMBER_KEY])
            .and_then(|n| n.value.clone())
            .ok_or_else(unreadable)?;
        for path in [
            vec![vec![INDEX_06], key.clone()],
            vec![vec![INDEX_07], key.clone()],
            vec![vec![INDEX_BY_KEY], by_key.clone(), key.clone()],
        ] {
            let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            index.remove(&path);
        }
        if index
            .at(&[&[INDEX_BY_KEY], &by_key])
            .is_some_and(|n| n.children.is_empty() && n.value.is_none())
        {
            index.remove(&[&[INDEX_BY_KEY], &by_key]);
        }
        removed += 1;
        changes.push((
            format!("[1f, 05, {}]", hex(&key)),
            format!("library item {item} removed: its last use goes"),
        ));
        members.push((key, None));
    }
    if removed > 0 {
        let index = index.as_mut().expect("an item was removed from it");
        let count = index
            .child(&[INDEX_COUNT])
            .and_then(|n| n.value.as_deref())
            .and_then(|v| <[u8; 4]>::try_from(v).ok())
            .map(u32::from_be_bytes)
            .and_then(|c| c.checked_sub(removed))
            .ok_or_else(|| {
                Refusal("The file library's item count does not read; nothing was written.".into())
            })?;
        index.set_child(OwnedNode::leaf(
            vec![INDEX_COUNT],
            count.to_be_bytes().to_vec(),
        ));
        changes.push((
            "[1f, 01]".into(),
            format!("{removed} items' index entries removed, {count} items left"),
        ));
    }
    Ok(Released {
        members,
        index: index.filter(|_| removed > 0),
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A library of items 1 (one use) and 2 (two uses), with their index.
    fn library() -> OwnedNode {
        let item = |id: u8, uses: u32, key: u8| {
            OwnedNode::list(
                vec![id],
                vec![
                    OwnedNode::leaf(vec![MEMBER_KEY], vec![key; 16]),
                    OwnedNode::leaf(vec![USES], uses.to_be_bytes().to_vec()),
                    OwnedNode::leaf(vec![0x77], vec![9]),
                    OwnedNode::leaf(MOD_COUNT.to_vec(), vec![1, 2]),
                ],
            )
        };
        let mut index = OwnedNode::list(vec![INDEX], Vec::new());
        index.set_child(OwnedNode::leaf(
            vec![INDEX_COUNT],
            2u32.to_be_bytes().to_vec(),
        ));
        for (id, key) in [(1u8, 0xaa), (2, 0xbb)] {
            index.put(
                &[&[INDEX_06], &[id]],
                OwnedNode::leaf(Vec::new(), Vec::new()),
            );
            index.put(
                &[&[INDEX_07], &[id]],
                OwnedNode::leaf(Vec::new(), Vec::new()),
            );
            index.put(
                &[&[INDEX_BY_KEY], &[key; 16], &[id]],
                OwnedNode::leaf(Vec::new(), Vec::new()),
            );
        }
        OwnedNode::list(
            vec![0x1f],
            vec![
                index,
                OwnedNode::list(vec![MEMBERS], vec![item(1, 1, 0xaa), item(2, 2, 0xbb)]),
            ],
        )
    }

    #[test]
    fn released_uses_lower_counts_and_remove_items_left_without_one() {
        let library = library();
        let released = release(&library, &[(1, 1), (2, 1)].into()).unwrap();
        // Item 2: one use left, its stamp count raised by the one released.
        let (key, two) = &released.members[1];
        assert_eq!(key, &[2]);
        let two = two.as_ref().unwrap();
        assert_eq!(two.child(&[USES]).unwrap().value, Some(vec![0, 0, 0, 1]));
        assert_eq!(two.child(&MOD_COUNT).unwrap().value, Some(vec![1, 3]));
        assert_eq!(two.child(&[0x77]).unwrap().value, Some(vec![9]));
        // Item 1 goes, with its index entries; the count is lowered.
        assert_eq!(released.members[0], (vec![1], None));
        let index = released.index.unwrap();
        assert!(index.at(&[&[INDEX_06], &[1]]).is_none());
        assert!(index.at(&[&[INDEX_07], &[1]]).is_none());
        assert!(index.at(&[&[INDEX_BY_KEY], &[0xaa; 16]]).is_none());
        assert!(index.at(&[&[INDEX_BY_KEY], &[0xbb; 16], &[2]]).is_some());
        assert_eq!(
            index.child(&[INDEX_COUNT]).unwrap().value,
            Some(1u32.to_be_bytes().to_vec())
        );
        // More uses than counted, or a missing item, are refused.
        assert!(release(&library, &[(1, 2)].into()).is_err());
        assert!(release(&library, &[(9, 1)].into()).is_err());
    }
}
