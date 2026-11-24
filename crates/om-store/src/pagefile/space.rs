//! An upper bound on the pages one entry's edits can make a tree write
//! (`docs/server-storage.md` section 6.5: space is reserved before an
//! entry is appended), for tree format version 1
//! (`docs/server-storage-format.md` sections 2.7 and 4.6).

use crate::page::{self, MAX_KEY, MAX_LEVEL};
use crate::tree::{key, Edit};

/// The overflow pages of the longest separator a split can make.
fn separator_pages() -> u64 {
    if page::separator_inline(MAX_KEY).is_some() {
        page::overflow_pages(MAX_KEY) as u64
    } else {
        0
    }
}

fn item_pages(key_len: usize, value_len: usize) -> u64 {
    if page::item_inline(key_len, value_len).is_some() {
        page::overflow_pages(key_len + value_len) as u64
    } else {
        0
    }
}

/// At most how many pages applying `edits` to a tree of `height` levels
/// (`tree::Tree::height`; 1 for a new tree) writes, under tree format
/// version 1. The bound counts:
///
/// - **primitives**: a put at depth `d` is at most `d` sets (the item, and
///   each ancestor it creates or turns from a value into both) and one
///   range delete (what was under the path); a delete is one range delete;
/// - **levels**: the tree's height, plus the levels the entry can add: a
///   root splits only when it overflows, which takes at least twice the
///   leaf splits of the split before it, so `s` sets add at most
///   `1 + log2(s)` levels; never more than [`MAX_LEVEL`] + 1;
/// - **per set**, at each level the page copied and one part split off it,
///   and the overflow pages of a new separator (one leaf split);
/// - **per range delete**, at each level the pages at both ends of the
///   range, a sibling each joins with and a part each join splits off
///   again, and the overflow pages of two new separators;
/// - **overflow pages** of every item set and every ancestor created;
/// - one page per level added (the new roots).
///
/// Pages are counted once per primitive even when several primitives
/// touch the same page, so the bound is loose, and the looser the larger
/// the entry: one put of a node at depth 1 into a tree of height 3 is
/// bounded by 45 pages, one at depth 3 by 82, and either writes 3. The
/// exact count is a function of the tree and the edits alone (section
/// 2.7's one shape per sequence of edits): `Reservation::taken` reports it
/// once the entry is applied, so an entry can carry it and recovery and
/// followers reserve exactly that.
pub fn entry_bound(edits: &[Edit], height: usize) -> u64 {
    let mut sets = 0u64;
    let mut deletes = 0u64;
    let mut overflow = 0u64;
    for edit in edits {
        match edit {
            Edit::Put { path, node } => {
                let depth = path.len().max(1) as u64;
                sets += depth;
                deletes += 1;
                overflow += item_pages(key::encoded_len(path), node.value().len());
                for d in 1..path.len() {
                    overflow += item_pages(key::encoded_len(&path[..d]), 0);
                }
            }
            Edit::Delete { .. } => deletes += 1,
        }
    }
    let grow = if sets > 0 {
        1 + u64::from(sets.ilog2())
    } else {
        0
    };
    let levels = (height.max(1) as u64 + grow).min(u64::from(MAX_LEVEL) + 1);
    let sep = separator_pages();
    sets * (2 * levels + sep) + deletes * (6 * levels + 2 * sep) + overflow + grow
}
