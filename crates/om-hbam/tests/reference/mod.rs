//! The bulk writer as it was before it streamed (`om_hbam::writer` at
//! the commit before `NodeWriter`), kept as an independent oracle: the
//! files whose acceptance by FileMaker hbam-writer.md 9 records were its
//! output, so the streaming writer must write the same bytes. Unchanged
//! but for two things: it takes the refusals from the public
//! `unwritable_in_tree`, which reports the first node in the same
//! pre-order its own check refused, and it is built from public items.

use om_hbam::container::{PageHeader, PAGE_HEADER_LEN};
use om_hbam::serial;
use om_hbam::slot::{encode_chunk, minimal_code, BEGIN, POP};
use om_hbam::writer::{unwritable_in_tree, MAX_DEPTH};
use om_hbam::{Error, OwnedNode, Result};

const PAGE_SIZE: usize = 4096;
const BODY: usize = PAGE_SIZE - PAGE_HEADER_LEN;
const MAX_INLINE_VALUE: usize = 1000;
const CLEAN_CLOSE: usize = 0x800;
const CLEAN: u8 = 0xac;
const SCN: usize = 0x802;
const LAST: u8 = 0xc0;
const FIRST_LEAF: u32 = 2;

/// One segment of a separator: a real key segment or one of the two fences
/// that sort below and above every real segment (section 1).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Begin,
    Key(Vec<u8>),
    End,
}

/// An item of the leaf stream (section 1).
enum Item {
    Begin,
    Pop,
    /// A push, always followed by `Begin`; the pair is one unit.
    Push(Vec<u8>),
    Leaf(Vec<u8>, Vec<u8>),
}

/// The leaf stream of `root`: `begin`, every key in order, `pop`. Values
/// over 1000 bytes become chunked nodes (2.3): child `00` = the u32 length,
/// then 1000-byte pieces keyed 1, 2, ... as serial keys (`7f`, then `80 00`
/// for piece 128; containers.md 1.5).
fn items(root: &OwnedNode) -> Result<Vec<Item>> {
    fn node(n: &OwnedNode, out: &mut Vec<Item>) -> Result<()> {
        let value = n.value.as_deref().unwrap_or_default();
        if n.push || !n.children.is_empty() {
            out.push(Item::Push(n.segment.clone()));
            out.push(Item::Begin);
            for child in &n.children {
                node(child, out)?;
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
    // The first node `refusal` refused, in the same pre-order.
    if let Some(refused) = unwritable_in_tree(root, 1).first() {
        return Err(Error::Write(refused.reason));
    }
    let mut out = vec![Item::Begin];
    for child in &root.children {
        node(child, &mut out)?;
    }
    out.push(Item::Pop);
    Ok(out)
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
