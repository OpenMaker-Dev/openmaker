//! RID lists: sets of record ids, the form of a found set in a query result
//! and of a table's record-id list at `[table, 13]` (spec
//! `docs/re/query-program.md` 5.1).
//!
//! A RID list node's children are leaves keyed by serial keys in increasing
//! order, each either a **singleton** (key = the record id, no value) or a
//! **bitmap run** (key = a base id that is a multiple of 32, value = `4k`
//! bytes of big-endian u32 words; bit `b` of word `j`, least significant
//! first, set means record `base + 32 j + b` is present). Readers accept any
//! mix of the two.

use std::collections::BTreeMap;
use std::ops::{BitOrAssign, Bound};

use crate::error::{Error, Result};
use crate::serial;
use crate::tree::{Node, OwnedNode};

/// Record ids per block of the in-memory set.
const BLOCK: u64 = 64;

/// A RID list entry keyed `key` that does not decode, for `reason`.
fn bad_entry(key: &[u8], reason: &'static str) -> Error {
    Error::BadValue {
        path: format!("{key:02x?}"),
        reason,
    }
}

/// A set of record ids.
///
/// Held as a bitmap of 64-id blocks (block `id / 64`, bit `id % 64`; no
/// empty blocks), so a set takes memory in proportion to its encoded form
/// rather than to the number of records: a dense run of record ids, which
/// a RID list's bitmap encodes in one bit per record, stays about that
/// size, and the set operations work a block at a time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RidList(BTreeMap<u64, u64>);

impl RidList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode a RID list node (its children are the entries).
    pub fn from_node(node: &Node<'_>) -> Result<Self> {
        Self::from_entries(
            node.children
                .iter()
                .map(|c| (c.segment.0, c.value.unwrap_or_default())),
        )
    }

    /// Decode an owned RID list node.
    pub fn from_owned(node: &OwnedNode) -> Result<Self> {
        Self::from_entries(
            node.children
                .iter()
                .map(|c| (c.segment.as_slice(), c.value.as_deref().unwrap_or_default())),
        )
    }

    fn from_entries<'e>(entries: impl Iterator<Item = (&'e [u8], &'e [u8])>) -> Result<Self> {
        let mut list = Self::new();
        for (key, value) in entries {
            list.add(key, value)?;
        }
        Ok(list)
    }

    /// Add the records of one entry of a RID list node, its key and value
    /// (a child of the node, as [`RidList::from_owned`] reads each), for a
    /// reader that has the entries one at a time rather than the node.
    pub fn add_entry(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.add(key, value)
    }

    /// [`RidList::add_entry`], inlined into the loops that decode a whole
    /// node: a call per entry there costs a find that reads many index
    /// entries several percent. A bitmap run is added by a call of its
    /// own ([`RidList::add_run`]), so that the loop stays small.
    #[inline(always)]
    fn add(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let id =
            serial::decode(key).ok_or_else(|| bad_entry(key, "RID key is not a serial key"))?;
        if value.is_empty() {
            self.insert(id);
            return Ok(());
        }
        self.add_run(key, id, value)
    }

    /// Add the bitmap run `value` keyed `key`, which decodes to `id`.
    #[inline(never)]
    fn add_run(&mut self, key: &[u8], id: u64, value: &[u8]) -> Result<()> {
        if id % 32 != 0 || value.len() % 4 != 0 {
            return Err(bad_entry(key, "RID bitmap run is misaligned"));
        }
        // Each nonzero word's records `start..=start + 31` must be ids:
        // checked before any word is added, so that a run refused leaves
        // the set as it was. A word's start grows with its place, so only
        // the last nonzero word can fail, and only when the run's last word
        // would; then that one is looked for and checked.
        let start = |j: usize| {
            (j as u64)
                .checked_mul(32)
                .and_then(|offset| id.checked_add(offset))
                .filter(|start| start.checked_add(31).is_some())
        };
        let passes = || bad_entry(key, "RID bitmap run passes the largest record id");
        if start(value.len() / 4 - 1).is_none()
            && value
                .chunks_exact(4)
                .rposition(|word| word != [0; 4])
                .is_some_and(|j| start(j).is_none())
        {
            return Err(passes());
        }
        for (j, word) in value.chunks_exact(4).enumerate() {
            let word = u32::from_be_bytes(word.try_into().expect("four bytes"));
            if word == 0 {
                continue;
            }
            // Checked above for every nonzero word.
            let start = start(j).ok_or_else(passes)?;
            // `start` is a multiple of 32: the word is one half of a block.
            *self.0.entry(start / BLOCK).or_default() |= u64::from(word) << (start % BLOCK);
        }
        Ok(())
    }

    /// The entries in key order, as FileMaker Server writes them
    /// (`docs/re/write-path.md` 5.3, checked against 11,786 captured lists):
    /// ids fall in windows of 124 u32 words (3,968 ids) aligned at multiples
    /// of 3,968. In each window a run starts at a non-zero word and ends
    /// before a gap of four or more zero words or at the window's end. A run
    /// of one word with one bit set is a singleton; any other run is a bitmap
    /// keyed by the id of its first word's bit 0.
    pub fn entries(&self) -> Vec<OwnedNode> {
        const WINDOW_WORDS: usize = 124;
        const WINDOW: u64 = 32 * WINDOW_WORDS as u64;
        /// A window is a whole number of blocks.
        const WINDOW_BLOCKS: u64 = WINDOW / BLOCK;
        let mut out = Vec::new();
        let mut blocks = self.0.iter().peekable();
        while let Some(&(&first, _)) = blocks.peek() {
            let window = first / WINDOW_BLOCKS;
            let base = window * WINDOW;
            let mut words = [0u32; WINDOW_WORDS];
            while let Some((&block, &bits)) = blocks.next_if(|&(&b, _)| b / WINDOW_BLOCKS == window)
            {
                let at = 2 * (block - window * WINDOW_BLOCKS) as usize;
                words[at] = bits as u32;
                words[at + 1] = (bits >> 32) as u32;
            }
            let mut w = 0;
            while w < WINDOW_WORDS {
                if words[w] == 0 {
                    w += 1;
                    continue;
                }
                let (start, mut end, mut zeros) = (w, w, 0);
                for (j, &word) in words.iter().enumerate().skip(w) {
                    if word == 0 {
                        zeros += 1;
                        if zeros >= 4 {
                            break;
                        }
                    } else {
                        zeros = 0;
                        end = j;
                    }
                }
                let run = &words[start..=end];
                let first = base + 32 * start as u64;
                if let [word] = run {
                    if word.count_ones() == 1 {
                        let id = first + u64::from(word.trailing_zeros());
                        out.push(OwnedNode::leaf(serial::encode(id), Vec::new()));
                        w = end + 1;
                        continue;
                    }
                }
                let value: Vec<u8> = run.iter().flat_map(|word| word.to_be_bytes()).collect();
                out.push(OwnedNode::leaf(serial::encode(first), value));
                w = end + 1;
            }
        }
        out
    }

    /// A RID list node keyed `segment`.
    pub fn to_node(&self, segment: impl Into<Vec<u8>>) -> OwnedNode {
        OwnedNode::list(segment, self.entries())
    }

    pub fn contains(&self, id: u64) -> bool {
        self.0
            .get(&(id / BLOCK))
            .is_some_and(|bits| bits & 1 << (id % BLOCK) != 0)
    }

    pub fn insert(&mut self, id: u64) -> bool {
        let bits = self.0.entry(id / BLOCK).or_default();
        let bit = 1 << (id % BLOCK);
        let new = *bits & bit == 0;
        *bits |= bit;
        new
    }

    pub fn len(&self) -> usize {
        self.0.values().map(|bits| bits.count_ones() as usize).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The number of 64-id blocks the set holds: what its memory and its set
    /// operations cost, in proportion to its encoded form.
    pub fn blocks(&self) -> usize {
        self.0.len()
    }

    /// The records between `low` and `high`, reading only the blocks in
    /// that range.
    pub fn within(&self, low: Bound<u64>, high: Bound<u64>) -> Self {
        let low = match low {
            Bound::Included(id) => Some(id),
            Bound::Excluded(id) => id.checked_add(1),
            Bound::Unbounded => Some(0),
        };
        let high = match high {
            Bound::Included(id) => Some(id),
            Bound::Excluded(id) => id.checked_sub(1),
            Bound::Unbounded => Some(u64::MAX),
        };
        let (Some(low), Some(high)) = (low, high) else {
            return Self::new();
        };
        if low > high {
            return Self::new();
        }
        let (first, last) = (low / BLOCK, high / BLOCK);
        Self(
            self.0
                .range(first..=last)
                .filter_map(|(&block, &bits)| {
                    let mut mask = u64::MAX;
                    if block == first {
                        mask &= u64::MAX << (low % BLOCK);
                    }
                    if block == last {
                        mask &= u64::MAX >> (BLOCK - 1 - high % BLOCK);
                    }
                    let kept = bits & mask;
                    (kept != 0).then_some((block, kept))
                })
                .collect(),
        )
    }

    pub fn iter(&self) -> impl Iterator<Item = u64> + '_ {
        self.0.iter().flat_map(|(&block, &bits)| {
            let mut rest = bits;
            std::iter::from_fn(move || {
                (rest != 0).then(|| {
                    let bit = rest.trailing_zeros();
                    rest &= rest - 1;
                    block * BLOCK + u64::from(bit)
                })
            })
        })
    }

    /// Records in both lists.
    pub fn and(&self, other: &Self) -> Self {
        let (small, large) = if self.0.len() <= other.0.len() {
            (self, other)
        } else {
            (other, self)
        };
        Self(
            small
                .0
                .iter()
                .filter_map(|(&block, &bits)| {
                    let both = bits & large.0.get(&block)?;
                    (both != 0).then_some((block, both))
                })
                .collect(),
        )
    }

    /// Records in either list.
    pub fn or(&self, other: &Self) -> Self {
        let (small, large) = if self.0.len() <= other.0.len() {
            (self, other)
        } else {
            (other, self)
        };
        let mut out = large.clone();
        for (&block, &bits) in &small.0 {
            *out.0.entry(block).or_default() |= bits;
        }
        out
    }

    /// Records in this list but not the other.
    pub fn and_not(&self, other: &Self) -> Self {
        Self(
            self.0
                .iter()
                .filter_map(|(&block, &bits)| {
                    let rest = bits & !other.0.get(&block).copied().unwrap_or_default();
                    (rest != 0).then_some((block, rest))
                })
                .collect(),
        )
    }
}

/// Add the other list's records to this one, a block at a time: merging
/// many lists into one this way reads each once, where `a = a.or(b)` in a
/// loop copies the growing result for every list.
impl BitOrAssign<&RidList> for RidList {
    fn bitor_assign(&mut self, other: &RidList) {
        for (&block, &bits) in &other.0 {
            *self.0.entry(block).or_default() |= bits;
        }
    }
}

impl FromIterator<u64> for RidList {
    fn from_iter<I: IntoIterator<Item = u64>>(iter: I) -> Self {
        let mut list = Self::new();
        for id in iter {
            list.insert(id);
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bitmap run whose second nonzero word passes the largest record id
    /// is refused and leaves the set as it was: its first word, which fits,
    /// is not added first.
    #[test]
    fn a_refused_run_adds_nothing() {
        let mut list: RidList = [5].into_iter().collect();
        let base = u64::MAX - 31;
        let key = serial::encode(base);
        let refused = list.add_entry(&key, &[0, 0, 0, 1, 0, 0, 0, 1]);
        assert!(
            matches!(refused, Err(Error::BadValue { .. })),
            "{refused:?}"
        );
        assert_eq!(list, [5].into_iter().collect());
        // The same run with its second word empty is one record.
        list.add_entry(&key, &[0, 0, 0, 1, 0, 0, 0, 0]).unwrap();
        assert_eq!(list.iter().collect::<Vec<_>>(), [5, base]);
    }

    fn decode(entries: &[(&[u8], &[u8])]) -> RidList {
        RidList::from_entries(entries.iter().copied()).unwrap()
    }

    #[test]
    fn spec_examples() {
        // `8180 = 3c000000`: records 538..541.
        let list = decode(&[(&[0x81, 0x80], &[0x3c, 0, 0, 0])]);
        assert_eq!(list.iter().collect::<Vec<_>>(), [538, 539, 540, 541]);
        // `00 = 0000000e`: records 1, 2, 3.
        let list = decode(&[(&[0x00], &[0, 0, 0, 0x0e])]);
        assert_eq!(list.iter().collect::<Vec<_>>(), [1, 2, 3]);
        // `8200 = 80000000 00008000 00100201`: five records.
        let list = decode(&[(
            &[0x82, 0x00],
            &[0x80, 0, 0, 0, 0, 0, 0x80, 0, 0, 0x10, 0x02, 0x01],
        )]);
        assert_eq!(list.len(), 5);
        // A singleton: `8f dd`.
        let list = decode(&[(&[0x8f, 0xdd], &[])]);
        assert_eq!(
            list.iter().collect::<Vec<_>>(),
            [serial::decode(&[0x8f, 0xdd]).unwrap()]
        );
    }

    #[test]
    fn encoding_round_trips() {
        for ids in [
            vec![1],
            vec![1, 2, 3],
            vec![538, 539, 540, 541],
            vec![5, 900, 901, 2000, 70_000],
            (1..=200).collect::<Vec<_>>(),
            vec![31, 32],
        ] {
            let list: RidList = ids.iter().copied().collect();
            let node = list.to_node(vec![2]);
            assert_eq!(RidList::from_owned(&node).unwrap(), list, "{ids:?}");
        }
        // The forms FileMaker Server wrote for these sets.
        let list: RidList = [1, 2, 3].into_iter().collect();
        assert_eq!(
            list.entries(),
            [OwnedNode::leaf(vec![0], vec![0, 0, 0, 0x0e])]
        );
        let list: RidList = [538, 539, 540, 541].into_iter().collect();
        assert_eq!(
            list.entries(),
            [OwnedNode::leaf(vec![0x81, 0x80], vec![0x3c, 0, 0, 0])]
        );
        let list: RidList = [1].into_iter().collect();
        assert_eq!(list.entries(), [OwnedNode::leaf(vec![1], vec![])]);
        let list: RidList = [1, 2, 3, 10].into_iter().collect();
        assert_eq!(
            list.entries(),
            [OwnedNode::leaf(vec![0], vec![0, 0, 0x04, 0x0e])]
        );
    }

    #[test]
    fn runs_follow_filemaker_servers_writer_rule() {
        // Words 0 and 3 (a gap of two zero words) form one run.
        let list: RidList = [1, 100].into_iter().collect();
        assert_eq!(
            list.entries(),
            [OwnedNode::leaf(
                vec![0],
                [[0, 0, 0, 2], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0x10]].concat()
            )]
        );
        // A gap of four zero words splits it: two singletons.
        let list: RidList = [1, 160].into_iter().collect();
        assert_eq!(
            list.entries(),
            [
                OwnedNode::leaf(vec![1], vec![]),
                OwnedNode::leaf(serial::encode(160), vec![]),
            ]
        );
        // Runs never cross a 3,968-id window.
        let list: RidList = [3967, 3968].into_iter().collect();
        assert_eq!(
            list.entries(),
            [
                OwnedNode::leaf(serial::encode(3967), vec![]),
                OwnedNode::leaf(serial::encode(3968), vec![]),
            ]
        );
        // One word with two records is a bitmap keyed at the word.
        let list: RidList = [64, 65].into_iter().collect();
        assert_eq!(
            list.entries(),
            [OwnedNode::leaf(serial::encode(64), vec![0, 0, 0, 3])]
        );
    }

    #[test]
    fn rejects_misaligned_runs() {
        assert!(RidList::from_entries([(&[0x05][..], &[0, 0, 0, 1][..])].into_iter()).is_err());
        // A run that would name records past the largest id.
        let last = serial::encode(u64::MAX - 31);
        assert!(
            RidList::from_entries([(&last[..], &[0, 0, 0, 1, 0, 0, 0, 1][..])].into_iter())
                .is_err()
        );
    }

    #[test]
    fn set_algebra() {
        let a: RidList = [1, 2, 3, 4].into_iter().collect();
        let b: RidList = [3, 4, 5].into_iter().collect();
        assert_eq!(a.and(&b).iter().collect::<Vec<_>>(), [3, 4]);
        assert_eq!(a.or(&b).len(), 5);
        assert_eq!(a.and_not(&b).iter().collect::<Vec<_>>(), [1, 2]);
        let mut c = a.clone();
        c |= &b;
        assert_eq!(c, a.or(&b));
        // Ranges at block edges, empty ones and the largest ids.
        let d: RidList = [0, 63, 64, 127, 128, 500, u64::MAX].into_iter().collect();
        let within = |low, high| d.within(low, high).iter().collect::<Vec<_>>();
        use Bound::{Excluded, Included, Unbounded};
        assert_eq!(within(Included(63), Included(128)), [63, 64, 127, 128]);
        assert_eq!(within(Excluded(63), Excluded(128)), [64, 127]);
        assert_eq!(within(Unbounded, Excluded(64)), [0, 63]);
        assert_eq!(within(Excluded(500), Unbounded), [u64::MAX]);
        assert_eq!(within(Included(129), Included(499)), []);
        assert_eq!(within(Included(5), Included(4)), []);
        assert_eq!(within(Excluded(u64::MAX), Unbounded), []);
        assert_eq!(within(Unbounded, Excluded(0)), []);
        assert_eq!(within(Unbounded, Unbounded).len(), d.len());
    }
}
