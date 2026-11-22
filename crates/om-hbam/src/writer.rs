//! Writing a whole file from a key tree: the bulk rebuild of
//! `docs/re/hbam-writer.md` 7.2.
//!
//! The tree's chunk stream is cut into leaf pages 2, 3, ... (each re-pushing
//! the key path open at its start), the index is built bottom-up from each
//! page's first key, its top page becomes the root at page 1, and the other
//! index pages follow the leaves. Every page is stamped with a fresh SCN and
//! the superblock is marked closed cleanly. FileMaker's consistency check
//! accepts this shape, and it is the shape of FileMaker's own compacted
//! copies.

use std::cmp::Ordering;

use crate::container::{PageHeader, PAGE_HEADER_LEN};
use crate::error::{Error, Result};
use crate::serial;
use crate::slot::{encode_chunk, minimal_code, BEGIN, POP};
use crate::tree::OwnedNode;

/// The page size FileMaker writes.
const PAGE_SIZE: usize = 4096;
/// Usable body bytes per page.
const BODY: usize = PAGE_SIZE - PAGE_HEADER_LEN;
/// The longest value stored inline; longer ones are chunked (2.3).
const MAX_INLINE_VALUE: usize = 1000;
/// The longest key segment a file can hold (a length byte below `0xDC`,
/// 2.2). Segments are at least one byte long.
pub const MAX_SEGMENT: usize = 219;
/// The deepest key path a written file may hold (16 segments, 2.3): a leaf
/// may sit at depth 16, a list or chunked value at depth 15 at most. The
/// one definition of this limit: [`unwritable`] and `om-query`'s program
/// bounds use it too.
pub const MAX_DEPTH: usize = 16;
/// Superblock fields (5.1): the clean-close flag and the file SCN.
const CLEAN_CLOSE: usize = 0x800;
const CLEAN: u8 = 0xac;
const SCN: usize = 0x802;
/// Index-page flag marking the last slot of a level (3.1).
const LAST: u8 = 0xc0;
/// The first page of the leaves; page 1 is the root.
const FIRST_LEAF: u32 = 2;

/// One segment of a separator: a real key segment or one of the two fences
/// that sort below and above every real segment (section 1).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Begin,
    Key(Vec<u8>),
    End,
}

impl Ord for Segment {
    fn cmp(&self, other: &Self) -> Ordering {
        use Segment::*;
        match (self, other) {
            (Begin, Begin) | (End, End) => Ordering::Equal,
            (Begin, _) | (_, End) => Ordering::Less,
            (_, Begin) | (End, _) => Ordering::Greater,
            (Key(a), Key(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Segment {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An item of the leaf stream (section 1).
enum Item {
    Begin,
    Pop,
    /// A push, always followed by `Begin`; the pair is one unit.
    Push(Vec<u8>),
    Leaf(Vec<u8>, Vec<u8>),
}

/// Why the writer cannot write node `n` itself at `depth` (1 for a child
/// of the root), its open key path taking `open` bytes when a page re-pushes
/// it (2.1); its children are checked on their own. A page holds the
/// re-pushed path and at least one item, so an item that does not fit
/// after its open path fits on no page.
fn refusal(n: &OwnedNode, depth: usize, open: usize) -> Option<&'static str> {
    if n.segment.is_empty() || n.segment.len() > MAX_SEGMENT || depth > MAX_DEPTH {
        return Some("key segment length or depth out of range");
    }
    let value = n.value.as_deref().unwrap_or_default();
    // A push and its begin are one unit.
    let opened = open + push_len(&n.segment);
    if n.push || !n.children.is_empty() {
        if !value.is_empty() {
            return Some("a list key cannot carry a value");
        }
        if depth == MAX_DEPTH {
            return Some("a list at the deepest level");
        }
        if opened + 1 > BODY {
            return Some("a key path too long to paginate");
        }
    } else if value.len() > MAX_INLINE_VALUE {
        if depth == MAX_DEPTH {
            return Some("a chunked value at the deepest level");
        }
        let Ok(pieces) = u32::try_from(value.len().div_ceil(MAX_INLINE_VALUE)) else {
            return Some("value over 4 GiB");
        };
        if u32::try_from(value.len()).is_err() {
            return Some("value over 4 GiB");
        }
        // The longest piece key is the last one's.
        let piece = leaf_len(&serial::encode(u64::from(pieces)), MAX_INLINE_VALUE);
        if opened + 1 > BODY || opened + piece > BODY {
            return Some("a key path too long to paginate");
        }
    } else if open + leaf_len(&n.segment, value.len()) > BODY {
        return Some("a key path too long to paginate");
    }
    None
}

/// The leaf stream of `root`: `begin`, every key in order, `pop`. Values
/// over 1000 bytes become chunked nodes (2.3): child `00` = the u32 length,
/// then 1000-byte pieces keyed 1, 2, ... as serial keys (`7f`, then `80 00`
/// for piece 128; containers.md 1.5).
fn items(root: &OwnedNode) -> Result<Vec<Item>> {
    fn node(n: &OwnedNode, depth: usize, open: usize, out: &mut Vec<Item>) -> Result<()> {
        if let Some(reason) = refusal(n, depth, open) {
            return Err(Error::Write(reason));
        }
        let value = n.value.as_deref().unwrap_or_default();
        if n.push || !n.children.is_empty() {
            out.push(Item::Push(n.segment.clone()));
            out.push(Item::Begin);
            let open = open + push_len(&n.segment);
            for child in &n.children {
                node(child, depth + 1, open, out)?;
            }
            out.push(Item::Pop);
        } else if value.len() > MAX_INLINE_VALUE {
            out.push(Item::Push(n.segment.clone()));
            out.push(Item::Begin);
            let total = u32::try_from(value.len()).map_err(|_| Error::Write("value over 4 GiB"))?;
            out.push(Item::Leaf(vec![0], total.to_be_bytes().to_vec()));
            for (i, piece) in value.chunks(MAX_INLINE_VALUE).enumerate() {
                out.push(Item::Leaf(serial::encode(i as u64 + 1), piece.to_vec()));
            }
            out.push(Item::Pop);
        } else {
            out.push(Item::Leaf(n.segment.clone(), value.to_vec()));
        }
        Ok(())
    }
    let mut out = vec![Item::Begin];
    for child in &root.children {
        node(child, 1, 0, &mut out)?;
    }
    out.push(Item::Pop);
    Ok(out)
}

/// A node [`rebuild`] cannot write: its key path and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unwritable {
    pub path: Vec<Vec<u8>>,
    pub reason: &'static str,
}

/// The nodes of `node`, placed at key path `parents` + its own segment,
/// that [`rebuild`] cannot write (the ancestors in `parents` included, as
/// lists), in key order; a refused node's children are not visited. Empty
/// when the subtree can be stored: a writer refuses exactly what this
/// reports (hbam-writer.md 2.2, 2.3, 2.1). Stops after `limit` nodes.
pub fn unwritable(parents: &[&[u8]], node: &OwnedNode, limit: usize) -> Vec<Unwritable> {
    fn visit(
        n: &OwnedNode,
        path: &mut Vec<Vec<u8>>,
        open: usize,
        limit: usize,
        out: &mut Vec<Unwritable>,
    ) {
        if out.len() >= limit {
            return;
        }
        path.push(n.segment.clone());
        if let Some(reason) = refusal(n, path.len(), open) {
            out.push(Unwritable {
                path: path.clone(),
                reason,
            });
        } else if n.push || !n.children.is_empty() {
            let open = open + push_len(&n.segment);
            for child in &n.children {
                visit(child, path, open, limit, out);
            }
        }
        path.pop();
    }
    let mut out = Vec::new();
    let mut path = Vec::new();
    let mut open = 0;
    for parent in parents {
        let list = OwnedNode {
            push: true,
            ..OwnedNode::list(parent.to_vec(), Vec::new())
        };
        path.push(parent.to_vec());
        if let Some(reason) = refusal(&list, path.len(), open) {
            out.push(Unwritable {
                path: path.clone(),
                reason,
            });
            return out;
        }
        open += push_len(parent);
    }
    visit(node, &mut path, open, limit, &mut out);
    out
}

/// Whether `node` holds a value: an inline one, or one chunked into pieces
/// (2.3, as a file stores a value over 1000 bytes). A key is either a value
/// or a list (2.2: "Can't have both data and children"), so nothing may be
/// put beneath a node that holds a value: it would become a list that
/// carries a value, which no file holds, or gain children among its pieces,
/// changing the value.
pub fn holds_value(node: &OwnedNode) -> bool {
    node.value.as_deref().is_some_and(|value| !value.is_empty())
        || (node.value.is_none() && !node.children.is_empty() && node.assembled().is_some())
}

/// The path of the first node along `path` in `root`, the node at `path`
/// included, that [`holds_value`]: a write that creates or extends lists
/// through `path` (as `OwnedNode::ensure` and `OwnedNode::put` do) would
/// change it. `None` when the path passes through lists and missing nodes
/// only.
pub fn value_on_path(root: &OwnedNode, path: &[&[u8]]) -> Option<Vec<Vec<u8>>> {
    let mut node = root;
    for (depth, segment) in path.iter().enumerate() {
        node = node.child(segment)?;
        if holds_value(node) {
            return Some(path[..=depth].iter().map(|s| s.to_vec()).collect());
        }
    }
    None
}

/// What putting `node` (named by its own segment) beneath `parents` in
/// `root` would leave that [`rebuild`] cannot write or that changes a value
/// the put does not name, checked against the tree as it is, as
/// `OwnedNode::put` puts it (missing parents become lists, existing ones
/// stay or become lists): an existing parent that [`holds_value`], then
/// what [`unwritable`] reports for the node under its parents. Empty when
/// the put is safe. The node that `node` replaces is the put's to change.
pub fn put_refusals(
    root: &OwnedNode,
    parents: &[&[u8]],
    node: &OwnedNode,
    limit: usize,
) -> Vec<Unwritable> {
    if let Some(path) = value_on_path(root, parents) {
        return vec![Unwritable {
            path,
            reason: "a value cannot become a list",
        }];
    }
    unwritable(parents, node, limit)
}

/// Every node of `root`, a file's whole key tree, that [`rebuild`] cannot
/// write ([`unwritable`]), at most `limit` of them.
pub fn unwritable_in_tree(root: &OwnedNode, limit: usize) -> Vec<Unwritable> {
    let mut out = Vec::new();
    for child in &root.children {
        out.extend(unwritable(&[], child, limit - out.len()));
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// The bytes a chunk's key takes: a length byte unless it is 1 to 3 bytes
/// long (the size classes of `minimal_code`).
fn key_len(segment: &[u8]) -> usize {
    match segment.len() {
        n @ 1..=3 => n,
        n => 1 + n,
    }
}

/// The length of [`push_chunk`] of `segment`.
fn push_len(segment: &[u8]) -> usize {
    1 + key_len(segment)
}

/// The length of [`leaf_chunk`] of `segment` with a value of `value` bytes
/// (at most 65,535).
fn leaf_len(segment: &[u8], value: usize) -> usize {
    let size = match value {
        0..=2 | 4 | 6 | 8 => 0,
        3..=255 => 1,
        _ => 2,
    };
    1 + key_len(segment) + size + value
}

fn push_chunk(segment: &[u8]) -> Vec<u8> {
    let code = minimal_code(true, segment.len(), None).expect("segment length checked");
    encode_chunk(code, segment, &[]).expect("push chunk")
}

fn leaf_chunk(segment: &[u8], value: &[u8]) -> Vec<u8> {
    let size = (!value.is_empty()).then_some(value.len());
    let code = minimal_code(false, segment.len(), size).expect("sizes checked");
    encode_chunk(code, segment, value).expect("leaf chunk")
}

/// A page being built: its level, body and first key.
struct Built {
    level: u16,
    body: Vec<u8>,
    first: Vec<Segment>,
}

/// Cut the leaf stream into pages (2.1), recording each page's first key.
fn leaf_pages(items: &[Item]) -> Result<Vec<Built>> {
    // The open path: `None` for the root list, the segment of each node.
    let mut open: Vec<Option<Vec<u8>>> = Vec::new();
    let mut pages: Vec<Built> = Vec::new();
    let mut current: Option<Built> = None;
    let mut i = 0;
    while i < items.len() {
        // The next unit: a push with its begin, or one item.
        let (unit, len) = match &items[i] {
            Item::Push(segment) => ([push_chunk(segment), vec![BEGIN]].concat(), 2),
            Item::Begin => (vec![BEGIN], 1),
            Item::Pop => (vec![POP], 1),
            Item::Leaf(segment, value) => (leaf_chunk(segment, value), 1),
        };
        let fits = current
            .as_ref()
            .is_some_and(|page| page.body.len() + unit.len() <= BODY);
        if !fits {
            if let Some(page) = current.take() {
                pages.push(page);
            }
            let path: Vec<Vec<u8>> = open.iter().flatten().cloned().collect();
            let mut body: Vec<u8> = path.iter().flat_map(|s| push_chunk(s)).collect();
            let mut first: Vec<Segment> = path.iter().cloned().map(Segment::Key).collect();
            match &items[i] {
                Item::Leaf(segment, _) => first.push(Segment::Key(segment.clone())),
                Item::Push(segment) => {
                    first.push(Segment::Key(segment.clone()));
                    first.push(Segment::Begin);
                }
                Item::Begin => first.push(Segment::Begin),
                Item::Pop => first.push(Segment::End),
            }
            if body.len() + unit.len() > BODY {
                return Err(Error::Write("a key path too long to paginate"));
            }
            body.reserve(BODY - body.len());
            current = Some(Built {
                level: 0,
                body,
                first,
            });
        }
        current.as_mut().expect("a page is open").body.extend(unit);
        match &items[i] {
            Item::Push(segment) => open.push(Some(segment.clone())),
            Item::Begin => open.push(None),
            Item::Pop => {
                open.pop();
            }
            Item::Leaf(..) => {}
        }
        i += len;
    }
    pages.extend(current);
    Ok(pages)
}

/// The bytes of a separator segment in an index slot: key class and key
/// bytes (a fence is class 3, length 0, then its sentinel; 3.1).
fn index_key(segment: &Segment) -> (u8, Vec<u8>) {
    match segment {
        Segment::Begin => (3, vec![0x00, 0x01]),
        Segment::End => (3, vec![0x00, 0xff]),
        Segment::Key(bytes) => match bytes.len() {
            1 => (0, bytes.clone()),
            2 => (1, bytes.clone()),
            3 => (2, bytes.clone()),
            n => (3, [&[n as u8][..], bytes].concat()),
        },
    }
}

/// One index page's body: its entries as a prefix trie (3.1).
fn index_body(entries: &[(Vec<Segment>, u32)]) -> Vec<u8> {
    fn level(entries: &[(Vec<Segment>, u32)], depth: usize, out: &mut Vec<u8>) {
        let mut groups: Vec<&[(Vec<Segment>, u32)]> = Vec::new();
        let mut start = 0;
        for i in 1..=entries.len() {
            if i == entries.len() || entries[i].0[depth] != entries[start].0[depth] {
                groups.push(&entries[start..i]);
                start = i;
            }
        }
        for (g, group) in groups.iter().enumerate() {
            let last = if g + 1 == groups.len() { LAST } else { 0 };
            let (class, key) = index_key(&group[0].0[depth]);
            if let [(separator, page)] = group {
                if separator.len() == depth + 1 {
                    out.push(last | class << 3 | 3);
                    out.extend(key);
                    out.extend(page.to_be_bytes());
                    continue;
                }
            }
            out.push(last | 0x20 | class << 3);
            out.extend(key);
            level(group, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    level(entries, 0, &mut out);
    out
}

/// Pack `entries` (in key order) into index pages, each as full as fits.
fn index_pages(entries: &[(Vec<Segment>, u32)], level: u16) -> Result<Vec<Built>> {
    let mut pages = Vec::new();
    let mut start = 0;
    while start < entries.len() {
        let mut end = start + 1;
        if index_body(&entries[start..end]).len() > BODY {
            return Err(Error::Write("an index entry too long for a page"));
        }
        // Adding an ordered separator only adds trie bytes (the LAST flag
        // may move, but does not change the length). Bracket the first
        // non-fitting prefix, then find the largest fitting one. Rebuilding
        // every successive prefix otherwise makes page filling quadratic.
        let mut upper = (start + 2).min(entries.len());
        while end < entries.len() {
            if index_body(&entries[start..upper]).len() > BODY {
                break;
            }
            end = upper;
            upper = start + (end - start).saturating_mul(2).min(entries.len() - start);
        }
        while end + 1 < upper {
            let middle = end + (upper - end) / 2;
            if index_body(&entries[start..middle]).len() <= BODY {
                end = middle;
            } else {
                upper = middle;
            }
        }
        pages.push(Built {
            level,
            body: index_body(&entries[start..end]),
            first: entries[start].0.clone(),
        });
        start = end;
    }
    Ok(pages)
}

/// Write `root`, the whole key tree of a file, as a complete file. Page 0 is
/// `superblock` (the original file's page 0) with a new SCN and the
/// clean-close flag set.
pub fn rebuild(root: &OwnedNode, superblock: &[u8]) -> Result<Vec<u8>> {
    if superblock.len() != PAGE_SIZE {
        return Err(Error::Write("the superblock is not one 4096-byte page"));
    }
    let leaves = leaf_pages(&items(root)?)?;

    // Number the leaves from page 2, then build the index bottom-up; the top
    // index page is the root, page 1, the others follow the leaves.
    let mut pages: Vec<(u32, Built)> = Vec::new();
    let mut next = FIRST_LEAF;
    let mut entries = Vec::new();
    for page in leaves {
        entries.push((page.first.clone(), next));
        pages.push((next, page));
        next += 1;
    }
    let mut level = 1;
    let root_page = loop {
        let mut built = index_pages(&entries, level)?;
        if built.len() == 1 {
            break built.remove(0);
        }
        entries = Vec::new();
        for page in built {
            entries.push((page.first.clone(), next));
            pages.push((next, page));
            next += 1;
        }
        level += 1;
        if level > MAX_DEPTH as u16 {
            return Err(Error::Write("the index grew past 16 levels"));
        }
    };
    let last_page = next - 1;

    let mut scn = u64::from_be_bytes(superblock[SCN..SCN + 8].try_into().expect("8 bytes"));
    // Each page written takes the next change number (5.2): a stored one
    // without room for them all is refused, not wrapped.
    if scn.checked_add(pages.len() as u64 + 1).is_none() {
        return Err(Error::Write(
            "the file's change number has no room for the rebuilt pages",
        ));
    }
    let mut out = vec![0u8; (last_page as usize + 1) * PAGE_SIZE];
    let mut write_page = |number: u32, page: &Built, prev: u32, next: u32| {
        scn += 1;
        let header = PageHeader {
            log_seq_num: (scn >> 32) as u32,
            prev,
            next,
            level: page.level,
            free_space: (BODY - page.body.len()) as u16,
            mod_count: scn as u32,
        };
        let at = number as usize * PAGE_SIZE;
        out[at..at + PAGE_HEADER_LEN].copy_from_slice(&header.to_bytes());
        out[at + PAGE_HEADER_LEN..at + PAGE_HEADER_LEN + page.body.len()]
            .copy_from_slice(&page.body);
    };
    // Each level's pages are consecutive in `pages`; link them in order.
    for (i, (number, page)) in pages.iter().enumerate() {
        let same_level = |j: usize| pages.get(j).filter(|(_, p)| p.level == page.level);
        let prev = i.checked_sub(1).and_then(same_level).map_or(0, |(n, _)| *n);
        let next = same_level(i + 1).map_or(0, |(n, _)| *n);
        write_page(*number, page, prev, next);
    }
    // The root is alone at its level; its NextPage is the last page (4.1).
    write_page(1, &root_page, 0, last_page);

    out[..PAGE_SIZE].copy_from_slice(superblock);
    out[CLEAN_CLOSE] = CLEAN;
    out[CLEAN_CLOSE + 1] = 0;
    out[SCN..SCN + 8].copy_from_slice(&scn.to_be_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_tries_match_the_spec_example() {
        // The start of Contacts/en's root (3.1): BH -> 2, then entries under
        // 02 / 18 18 / 01 / "PNGf".
        let key = |parts: &[&[u8]]| -> Vec<Segment> {
            parts.iter().map(|p| Segment::Key(p.to_vec())).collect()
        };
        let png = |last: u8| key(&[&[0x02], &[0x18, 0x18], &[0x01], b"PNGf", &[last]]);
        let entries = vec![
            (vec![Segment::Begin], 2),
            (png(0x03), 78),
            (png(0x07), 79),
            (png(0x0b), 80),
        ];
        let body = index_body(&entries);
        let expected: Vec<u8> = [
            &[0x1b, 0x00, 0x01, 0, 0, 0, 2][..],
            &[0xe0, 0x02],
            &[0xe8, 0x18, 0x18],
            &[0xe0, 0x01],
            &[0xf8, 0x04, b'P', b'N', b'G', b'f'],
            &[0x03, 0x03, 0, 0, 0, 0x4e],
            &[0x03, 0x07, 0, 0, 0, 0x4f],
            &[0xc3, 0x0b, 0, 0, 0, 0x50],
        ]
        .concat();
        assert_eq!(body, expected);
    }

    #[test]
    fn index_page_boundaries_match_greedy_packing() {
        // Keep the original greedy packer as an independent boundary oracle.
        fn greedy(entries: &[(Vec<Segment>, u32)], level: u16) -> Vec<Built> {
            let mut pages = Vec::new();
            let mut start = 0;
            while start < entries.len() {
                let mut end = start + 1;
                assert!(index_body(&entries[start..end]).len() <= BODY);
                while end < entries.len() && index_body(&entries[start..=end]).len() <= BODY {
                    end += 1;
                }
                pages.push(Built {
                    level,
                    body: index_body(&entries[start..end]),
                    first: entries[start].0.clone(),
                });
                start = end;
            }
            pages
        }

        for width in [2, 4, MAX_SEGMENT] {
            for nested in [false, true] {
                // Include exact body capacity and its neighbors for the flat
                // two-byte (7 bytes/entry) and four-byte (10 bytes/entry) keys.
                for count in [0, 1, 2, 17, 18, 19, 406, 407, 408, 581, 582, 583, 2_000] {
                    let entries: Vec<_> = (0..count)
                        .map(|i: u32| {
                            let mut key = vec![0; width];
                            key[width - 2..].copy_from_slice(&(i as u16).to_be_bytes());
                            let path = if nested {
                                vec![
                                    Segment::Key(vec![(i / 100) as u8]),
                                    Segment::Key(vec![1, 2, 3]),
                                    Segment::Key(key),
                                    if i % 2 == 0 {
                                        Segment::Begin
                                    } else {
                                        Segment::End
                                    },
                                ]
                            } else {
                                vec![Segment::Key(key)]
                            };
                            (path, i + FIRST_LEAF)
                        })
                        .collect();
                    let expected = greedy(&entries, 3);
                    let actual = index_pages(&entries, 3).unwrap();
                    assert_eq!(
                        actual.len(),
                        expected.len(),
                        "width={width}, nested={nested}, count={count}"
                    );
                    for (actual, expected) in actual.iter().zip(&expected) {
                        assert_eq!(actual.level, expected.level);
                        assert_eq!(actual.first, expected.first);
                        assert_eq!(actual.body, expected.body);
                    }
                }
            }
        }
    }

    #[test]
    fn long_values_are_chunked_with_serial_piece_keys() {
        // 200,500 bytes: 201 pieces, so piece 128 is keyed `80 00`.
        let value: Vec<u8> = (0..200_500u32).map(|i| (i % 251) as u8).collect();
        let root = OwnedNode::list(Vec::new(), vec![OwnedNode::leaf(vec![0x05], value.clone())]);
        let superblock = {
            let mut page = vec![0u8; PAGE_SIZE];
            page[0x0f..0x14].copy_from_slice(crate::container::MAGIC);
            page[0x14..0x18].copy_from_slice(&(PAGE_SIZE as u32).to_be_bytes());
            page
        };
        let file = crate::HbamFile::from_bytes(rebuild(&root, &superblock).unwrap()).unwrap();
        let node = file.subtree(&[&[0x05]]).unwrap().unwrap();
        assert_eq!(node.children.len(), 202);
        assert_eq!(node.children[128].segment.0, [0x80, 0x00]);
        assert_eq!(node.assembled().unwrap().as_ref(), value.as_slice());
        // A stored change number with no room for the pages is refused.
        let mut full = superblock.clone();
        full[SCN..SCN + 8].copy_from_slice(&(u64::MAX - 3).to_be_bytes());
        assert!(rebuild(&root, &full).is_err());
    }

    #[test]
    fn unwritable_reports_what_rebuild_refuses() {
        // The page-room check counts chunk bytes as the encoder writes them.
        for key in 1..=MAX_SEGMENT {
            let segment = vec![7; key];
            assert_eq!(push_len(&segment), push_chunk(&segment).len());
            for value in (0..=20).chain([254, 255, 256, MAX_INLINE_VALUE]) {
                assert_eq!(
                    leaf_len(&segment, value),
                    leaf_chunk(&segment, &vec![1; value]).len()
                );
            }
        }
        let superblock = {
            let mut page = vec![0u8; PAGE_SIZE];
            page[0x0f..0x14].copy_from_slice(crate::container::MAGIC);
            page[0x14..0x18].copy_from_slice(&(PAGE_SIZE as u32).to_be_bytes());
            page
        };
        let leaf = |segment: Vec<u8>, value: usize| OwnedNode::leaf(segment, vec![3; value]);
        // A path of `depth` lists of `width`-byte segments ending in `last`.
        let deep = |depth: usize, width: usize, last: OwnedNode| {
            (1..depth).rev().fold(last, |child, d| {
                OwnedNode::list(vec![d as u8; width], vec![child])
            })
        };
        let cases = [
            (leaf(vec![1; MAX_SEGMENT], 0), true),
            (leaf(vec![1; MAX_SEGMENT + 1], 0), false),
            (leaf(Vec::new(), 1), false),
            (deep(MAX_DEPTH, 1, leaf(vec![1], 1)), true),
            (deep(MAX_DEPTH + 1, 1, leaf(vec![1], 1)), false),
            (deep(MAX_DEPTH, 1, leaf(vec![1], 1001)), false),
            (deep(MAX_DEPTH - 1, 1, leaf(vec![1], 5000)), true),
            // Fifteen 219-byte lists leave no page room for a 1000-byte
            // value under a long key, but do for a short value.
            (
                deep(MAX_DEPTH, MAX_SEGMENT, leaf(vec![1; MAX_SEGMENT], 1000)),
                false,
            ),
            (
                deep(MAX_DEPTH, MAX_SEGMENT, leaf(vec![1; MAX_SEGMENT], 10)),
                true,
            ),
            (deep(MAX_DEPTH - 1, MAX_SEGMENT, leaf(vec![1], 2000)), false),
        ];
        for (i, (node, writable)) in cases.into_iter().enumerate() {
            let root = OwnedNode::list(Vec::new(), vec![node.clone()]);
            let found = unwritable_in_tree(&root, 10);
            assert_eq!(found.is_empty(), writable, "case {i}: {found:?}");
            assert_eq!(rebuild(&root, &superblock).is_ok(), writable, "case {i}");
        }
        // Against a real tree: beneath a committed value, inline or chunked,
        // nothing may be put; beneath a list or a missing node, it may.
        let tree = OwnedNode::list(
            Vec::new(),
            vec![
                leaf(vec![1], 4),
                OwnedNode::list(
                    vec![2],
                    vec![
                        OwnedNode::leaf(vec![0], 3000u32.to_be_bytes().to_vec()),
                        leaf(vec![1], 1000),
                        leaf(vec![2], 1000),
                        leaf(vec![3], 1000),
                    ],
                ),
                OwnedNode::list(vec![3], vec![leaf(vec![1], 4)]),
            ],
        );
        for (parents, refused) in [
            (&[&[1u8][..]][..], true),
            (&[&[2u8][..]][..], true),
            (&[&[1u8][..], &[5u8][..]][..], true),
            (&[&[3u8][..]][..], false),
            (&[&[4u8][..], &[5u8][..]][..], false),
        ] {
            let found = put_refusals(&tree, parents, &leaf(vec![9], 1), 10);
            assert_eq!(!found.is_empty(), refused, "{parents:?}: {found:?}");
        }
        // A subtree under parents that are already too deep.
        let parents: Vec<&[u8]> = vec![&[1]; MAX_DEPTH];
        assert_eq!(
            unwritable(&parents, &leaf(vec![2], 1), 10)[0].path.len(),
            MAX_DEPTH
        );
    }

    #[test]
    fn fences_order_around_real_segments() {
        let k = |b: &[u8]| Segment::Key(b.to_vec());
        assert!(Segment::Begin < k(&[0]));
        assert!(k(&[0xff, 0xff]) < Segment::End);
        assert!(k(&[1]) < k(&[1, 0]));
    }
}
