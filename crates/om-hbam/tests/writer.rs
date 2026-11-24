//! The streaming writer (`writer::NodeWriter`, hbam-writer.md 7.2,
//! `docs/server-storage.md` 6.10) writes exactly the bytes of the bulk
//! writer it replaced, kept in `reference`, for generated trees of every
//! shape, refuses the trees that writer refused with the same reasons, and
//! reports a refused node when it is given.

mod reference;

use std::io::{Cursor, Read, Seek, Write};

use om_hbam::writer::{self, NodeWriter, MAX_DEPTH, MAX_SEGMENT};
use om_hbam::{HbamFile, OwnedNode, Result};

/// The change number a test file's page 0 holds: close below 2^32, so the
/// pages' change numbers cross into their `mLogSeqNum` half (5.2).
const SCN: u64 = (1 << 32) - 40;

/// A page 0 with the magic, the page size and change number `scn`.
fn superblock(scn: u64) -> Vec<u8> {
    let mut page = vec![0u8; 4096];
    page[0x0f..0x14].copy_from_slice(om_hbam::container::MAGIC);
    page[0x14..0x18].copy_from_slice(&4096u32.to_be_bytes());
    page[0x802..0x80a].copy_from_slice(&scn.to_be_bytes());
    page
}

/// Give `writer` the nodes below `root` as a depth-first walk gives them, a
/// node with children as a list.
fn give<W: Read + Write + Seek>(root: &OwnedNode, writer: &mut NodeWriter<W>) -> Result<()> {
    fn walk<'a, W: Read + Write + Seek>(
        node: &'a OwnedNode,
        path: &mut Vec<&'a [u8]>,
        writer: &mut NodeWriter<W>,
    ) -> Result<()> {
        path.push(&node.segment);
        let list = node.push || !node.children.is_empty();
        writer.node(path, list, node.value.as_deref())?;
        for child in &node.children {
            walk(child, path, writer)?;
        }
        path.pop();
        Ok(())
    }
    let mut path = Vec::new();
    root.children
        .iter()
        .try_for_each(|child| walk(child, &mut path, writer))
}

fn stream(root: &OwnedNode, superblock: &[u8]) -> Result<Vec<u8>> {
    let mut writer = NodeWriter::new(Cursor::new(Vec::new()), superblock)?;
    give(root, &mut writer)?;
    Ok(writer.finish()?.into_inner())
}

/// What a writer did: the file, or why it refused.
type Outcome = std::result::Result<Vec<u8>, String>;

fn outcome(written: Result<Vec<u8>>) -> Outcome {
    written.map_err(|e| e.to_string())
}

/// Where two outcomes differ, if they do (not the whole files: they can be
/// megabytes).
fn difference(actual: &Outcome, expected: &Outcome) -> Option<String> {
    match (actual, expected) {
        (Ok(a), Ok(b)) if a == b => None,
        (Ok(a), Ok(b)) => {
            let at = a.iter().zip(b).position(|(x, y)| x != y);
            Some(format!(
                "{} bytes, expected {}; first difference at {at:?} (page {:?})",
                a.len(),
                b.len(),
                at.map(|at| at / 4096)
            ))
        }
        (Err(a), Err(b)) if a == b => None,
        (a, b) => Some(format!(
            "{:?}, expected {:?}",
            a.as_ref().map(Vec::len),
            b.as_ref().map(Vec::len)
        )),
    }
}

/// `root` written by the bulk writer before it streamed, by `rebuild` and
/// by streaming its nodes: the same bytes or the same refusal. A file
/// written reads back as nodes that, streamed, write it again. Returns the
/// outcome.
fn check(name: &str, root: &OwnedNode, superblock: &[u8]) -> Outcome {
    let expected = outcome(reference::rebuild(root, superblock));
    for (writer, actual) in [
        ("rebuild", outcome(writer::rebuild(root, superblock))),
        ("stream", outcome(stream(root, superblock))),
    ] {
        if let Some(difference) = difference(&actual, &expected) {
            panic!("{name}, {writer}: {difference}");
        }
    }
    if let Ok(file) = &expected {
        let read = HbamFile::from_bytes(file.clone()).unwrap();
        let tree = read.key_tree().unwrap().root.to_owned_node();
        let again = outcome(stream(&tree, superblock));
        if let Some(difference) = difference(&again, &expected) {
            panic!("{name}, read back and streamed: {difference}");
        }
    }
    expected
}

/// The root page's level: how many index levels the file has.
fn index_levels(file: &[u8]) -> u16 {
    u16::from_be_bytes([file[4096 + 12], file[4096 + 13]])
}

/// A deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn range(&mut self, low: usize, high: usize) -> usize {
        low + self.below((high - low + 1) as u64) as usize
    }

    fn percent(&mut self, p: u64) -> bool {
        self.below(100) < p
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }

    /// A key segment: mostly the short ones files hold, some long, a few
    /// of the longest length.
    fn segment(&mut self) -> Vec<u8> {
        let len = match self.below(100) {
            0..=69 => self.range(1, 3),
            70..=89 => self.range(4, 16),
            90..=97 => self.range(17, MAX_SEGMENT - 1),
            _ => MAX_SEGMENT,
        };
        self.bytes(len)
    }

    /// A value: mostly short, some around the 1,000-byte split, some cut
    /// into many pieces.
    fn value(&mut self) -> Vec<u8> {
        let len = match self.below(100) {
            0..=39 => self.range(0, 8),
            40..=69 => self.range(9, 300),
            70..=84 => self.range(301, 999),
            85..=89 => self.range(1000, 1001),
            90..=98 => self.range(1002, 5000),
            _ => self.range(127_000, 130_000),
        };
        self.bytes(len)
    }

    /// The children of a node at `depth` (0 for the root), in key order,
    /// taking about `budget` bytes of keys and values. Lists thin out
    /// with depth, but now and then a chain dives to the deepest level.
    /// While `faults` lasts, about one node in 500 is one no file can
    /// hold.
    fn children(&mut self, depth: usize, budget: &mut i64, faults: &mut u32) -> Vec<OwnedNode> {
        let count = self.range(0, if depth == 0 { 40 } else { 16 });
        let mut children = Vec::new();
        for _ in 0..count {
            if *budget <= 0 {
                break;
            }
            let segment = self.segment();
            *budget -= segment.len() as i64;
            let list = match depth {
                0 => 60,
                1 | 2 => 30,
                _ => 12,
            };
            let node = if *faults > 0 && self.below(500) == 0 {
                *faults -= 1;
                match self.below(3) {
                    // A list carrying a value, a list at the deepest
                    // level, a chunked value there.
                    0 => OwnedNode {
                        value: Some(vec![1]),
                        ..OwnedNode::list(segment, Vec::new())
                    },
                    deepest => (depth + 1..MAX_DEPTH).rev().fold(
                        match deepest {
                            1 => OwnedNode::list(vec![1], Vec::new()),
                            _ => OwnedNode::leaf(vec![1], vec![0; 1001]),
                        },
                        |child, _| OwnedNode::list(vec![2], vec![child]),
                    ),
                }
            } else if depth + 1 < MAX_DEPTH && (self.percent(list) || self.percent(2)) {
                let deeper = if self.percent(4) {
                    // A dive: a list at each level, a value at the deepest.
                    let len = self.range(0, 40);
                    let leaf = OwnedNode::leaf(self.segment(), self.bytes(len));
                    (depth + 1..MAX_DEPTH).fold(leaf, |child, _| {
                        OwnedNode::list(self.segment(), vec![child])
                    })
                } else {
                    OwnedNode::list(Vec::new(), self.children(depth + 1, budget, faults))
                };
                OwnedNode::list(segment, deeper.children)
            } else {
                // A chunked value at the deepest level is a fault.
                let mut value = self.value();
                if depth + 1 == MAX_DEPTH {
                    value.truncate(1000);
                }
                *budget -= value.len() as i64;
                OwnedNode::leaf(segment, value)
            };
            children.push(node);
        }
        children.sort_by(|a, b| a.segment.cmp(&b.segment));
        children.dedup_by(|a, b| a.segment == b.segment);
        children
    }
}

/// Random trees of every shape: lists nested to the deepest level, the
/// longest segments, values on both sides of the 1,000-byte split and in
/// over 128 pieces, empty lists and empty values, and in every fourth tree
/// a node no file can hold. Each written, or refused, as the bulk writer
/// did.
#[test]
fn random_trees_are_written_as_the_bulk_writer_wrote_them() {
    let superblock = superblock(SCN);
    let (mut written, mut pages, mut deepest) = (0, 0, 0);
    for seed in 1..=60u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut budget = rng.range(2_000, 800_000) as i64;
        let mut faults = u32::from(seed % 4 == 0) * 3;
        let root = OwnedNode::list(Vec::new(), rng.children(0, &mut budget, &mut faults));
        if let Ok(file) = check(&format!("seed {seed}"), &root, &superblock) {
            written += 1;
            pages += file.len() / 4096;
            deepest = deepest.max(index_levels(&file));
        }
    }
    // Most are written, over thousands of pages and two index levels;
    // some are refused.
    assert!((40..60).contains(&written), "{written} of 60 written");
    assert!(
        pages > 3_000 && deepest >= 2,
        "{pages} pages, {deepest} levels"
    );
}

/// One table's records, eight 64-byte cells each (the shape of #673's
/// measurements): hundreds of leaf pages under a root of level 2.
fn records(count: u64) -> OwnedNode {
    let records = (1..=count)
        .map(|id| {
            let cells = (1..=8u8)
                .map(|field| OwnedNode::leaf(vec![field], vec![b'A' ^ 0x5a; 64]))
                .collect();
            OwnedNode::list(om_hbam::serial::encode(id), cells)
        })
        .collect();
    OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            vec![0x80, 0x01],
            vec![OwnedNode::list(vec![5], records)],
        )],
    )
}

/// `count` top-level lists of the longest segments, each holding a chain
/// of 13 more and, at its end, a 740-byte value under a longest key. Most
/// leaf pages start deep in a chain, so their first keys are thousands of
/// bytes long and an index page holds one or two of them: the index grows
/// about a level for each chain.
fn long_separators(count: u8) -> OwnedNode {
    let top = (0..count)
        .map(|i| {
            let leaf = OwnedNode::leaf(vec![0xee; MAX_SEGMENT], vec![7; 740]);
            let chained = (0..13).rev().fold(leaf, |child, depth| {
                OwnedNode::list(vec![depth as u8 + 1; MAX_SEGMENT], vec![child])
            });
            OwnedNode::list(vec![i; MAX_SEGMENT], vec![chained])
        })
        .collect();
    OwnedNode::list(Vec::new(), top)
}

/// Trees chosen for their shape: several index levels, the deepest paths of
/// the longest segments, every value size class and the piece counts
/// around 128, empty lists and values, an empty tree; and a tree whose
/// index needs more than 16 levels, which both refuse.
#[test]
fn shaped_trees_are_written_as_the_bulk_writer_wrote_them() {
    let superblock = superblock(SCN);
    let file = check("records", &records(6_000), &superblock).unwrap();
    assert_eq!(index_levels(&file), 2);
    assert!(file.len() > 700 * 4096, "{} pages", file.len() / 4096);

    let file = check("long separators", &long_separators(9), &superblock).unwrap();
    assert_eq!(index_levels(&file), 9);
    let file = check("16 index levels", &long_separators(16), &superblock).unwrap();
    assert_eq!(index_levels(&file), 16);
    let refused = check("17 index levels", &long_separators(20), &superblock);
    assert_eq!(
        refused.unwrap_err(),
        "cannot write the file: the index grew past 16 levels"
    );

    // Paths to the deepest level, of the longest segments, with siblings
    // at every level.
    let deep = |width: usize, value: usize| {
        (1..MAX_DEPTH - 1).rev().fold(
            OwnedNode::list(
                vec![0x40; width],
                vec![
                    OwnedNode::leaf(vec![0x01; width], vec![3; value]),
                    OwnedNode::leaf(vec![0x02; width], Vec::new()),
                ],
            ),
            |child, depth| {
                OwnedNode::list(
                    vec![depth as u8; width],
                    vec![
                        OwnedNode::leaf(vec![0], vec![depth as u8; 9]),
                        child,
                        OwnedNode::list(vec![0xff; width], Vec::new()),
                    ],
                )
            },
        )
    };
    for (width, value) in [(1, 1000), (4, 3), (MAX_SEGMENT, 10), (MAX_SEGMENT, 60)] {
        let root = OwnedNode::list(Vec::new(), vec![deep(width, value)]);
        check(&format!("deep {width} {value}"), &root, &superblock).unwrap();
    }

    // Every value size class, the 1,000-byte split, 127 to 129 pieces and a
    // value over 65,535 bytes; empty values and lists among them.
    let sizes = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 255, 256, 999, 1000, 1001, 1999, 2000, 2001, 65_535, 65_536,
        127_000, 128_000, 128_001, 200_500,
    ];
    let values = sizes
        .iter()
        .enumerate()
        .flat_map(|(i, &size)| {
            [
                OwnedNode::leaf(vec![i as u8 * 2], vec![size as u8; size]),
                OwnedNode::list(vec![i as u8 * 2 + 1], Vec::new()),
            ]
        })
        .collect();
    let root = OwnedNode::list(Vec::new(), vec![OwnedNode::list(vec![9], values)]);
    check("values", &root, &superblock).unwrap();

    for (name, root) in [
        ("empty", OwnedNode::list(Vec::new(), Vec::new())),
        (
            "an empty list",
            OwnedNode::list(Vec::new(), vec![OwnedNode::list(vec![1], Vec::new())]),
        ),
        (
            "an empty value",
            OwnedNode::list(Vec::new(), vec![OwnedNode::leaf(vec![1], Vec::new())]),
        ),
    ] {
        let file = check(name, &root, &superblock).unwrap();
        assert_eq!(file.len(), 3 * 4096, "{name}");
    }
}

/// The change numbers run out where the bulk writer said they did: the
/// last page written takes the largest one there is, and one more page is
/// refused.
#[test]
fn change_numbers_end_where_the_bulk_writer_said() {
    let root = OwnedNode::list(Vec::new(), vec![OwnedNode::leaf(vec![1], vec![2])]);
    // Two pages, the leaf and the root.
    let file = check("room", &root, &superblock(u64::MAX - 2)).unwrap();
    assert_eq!(file[0x802..0x80a], u64::MAX.to_be_bytes());
    let refused = check("no room", &root, &superblock(u64::MAX - 1));
    assert!(refused.unwrap_err().contains("change number"));
}

/// A file streamed into a `File` is the one written in memory: the pages
/// are read back and written at their places through a real file.
#[test]
fn a_file_target_holds_the_same_file() {
    let root = records(6_000);
    let superblock = superblock(SCN);
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("node-writer.fmp12");
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    let mut writer = NodeWriter::new(file, &superblock).unwrap();
    give(&root, &mut writer).unwrap();
    writer.finish().unwrap().sync_all().unwrap();
    let written = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(written == writer::rebuild(&root, &superblock).unwrap());
}

/// A target that gives back one byte altered whenever byte `offset` is
/// read, as a faulty device or cache could: what is stored stays right.
struct Altered {
    inner: Cursor<Vec<u8>>,
    offset: u64,
}

impl Read for Altered {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let start = self.inner.position();
        let read = self.inner.read(buf)?;
        if (start..start + read as u64).contains(&self.offset) {
            let at = (self.offset - start) as usize;
            buf[at] = buf[at].wrapping_add(1);
        }
        Ok(read)
    }
}

impl Write for Altered {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Seek for Altered {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

/// A page whose first key reads back other than it was written fails the
/// file, at the leaves and at an index level; read back as written, the
/// same target gives the file `rebuild` writes.
#[test]
fn a_page_read_back_altered_fails_the_file() {
    let superblock = superblock(SCN);
    let write = |root: &OwnedNode, offset: u64| {
        let target = Altered {
            inner: Cursor::new(Vec::new()),
            offset,
        };
        let mut writer = NodeWriter::new(target, &superblock)?;
        give(root, &mut writer)?;
        writer.finish().map(|target| target.inner.into_inner())
    };
    let refusal = |written: Result<Vec<u8>>| match written {
        Ok(file) => panic!("{} bytes written", file.len()),
        Err(e) => e.to_string(),
    };
    let body = |page: u64| page * 4096 + 20;
    // Five values of 1,000 bytes keyed 01 to 05: page 2 holds four, page 3
    // starts with 05. Read back as 06, the root would send 05 to page 2.
    let leaves = |count: u16, key: fn(u16) -> Vec<u8>| {
        let values = (1..=count)
            .map(|i| OwnedNode::leaf(key(i), vec![7; 1000]))
            .collect();
        OwnedNode::list(Vec::new(), values)
    };
    let five = leaves(5, |i| vec![i as u8]);
    let expected = writer::rebuild(&five, &superblock).unwrap();
    assert_eq!(expected[body(3) as usize + 1], 5);
    assert!(write(&five, u64::MAX).unwrap() == expected);
    let refused = refusal(write(&five, body(3) + 1));
    assert!(refused.ends_with("not the page written"), "{refused}");

    // 2,400 values under two-byte keys: 600 leaf pages, two level-1 pages
    // under a root of level 2. The second level-1 page's first key is read
    // back with its last byte altered, which keeps the root's entries in
    // order.
    let many = leaves(2400, |i| i.to_be_bytes().to_vec());
    let expected = writer::rebuild(&many, &superblock).unwrap();
    let root = &expected[body(1) as usize..];
    assert_eq!(index_levels(&expected), 2);
    // Root: terminal BH -> the first level-1 page, then the last terminal,
    // a two-byte key -> the second.
    assert_eq!((root[0], root[7]), (0x1b, 0xcb));
    let second = u32::from_be_bytes(root[10..14].try_into().unwrap());
    assert!(write(&many, u64::MAX).unwrap() == expected);
    let refused = refusal(write(&many, body(u64::from(second)) + 2));
    assert!(refused.ends_with("not the page written"), "{refused}");
}

/// A path, whether it opens a list, and its value.
type Given = (Vec<Vec<u8>>, bool, Option<&'static [u8]>);

/// A value cut into two pieces, and the longest inline one.
static BIG: [u8; 2000] = [5; 2000];
static SMALL: [u8; 1000] = [5; 1000];

/// Each refusal is reported at the node that causes it, with the reason
/// `rebuild` gives; the writer then refuses everything, `finish` included.
#[test]
fn refusals_are_reported_at_their_node() {
    let superblock = superblock(SCN);
    let long = vec![9; MAX_SEGMENT];
    // The path of `count` nested lists of `width`-byte segments, then
    // `last`.
    let under = |count: usize, width: usize, last: Option<Vec<u8>>| -> Vec<Vec<u8>> {
        (1..=count)
            .map(|depth| vec![depth as u8; width])
            .chain(last)
            .collect()
    };
    // Those lists, then the node `last`.
    let at = |count: usize, width: usize, last: Given| -> Vec<Given> {
        let mut nodes: Vec<Given> = (1..=count)
            .map(|depth| (under(depth, width, None), true, None))
            .collect();
        nodes.push(last);
        nodes
    };
    let leaf = |path: &[u8]| (vec![path.to_vec()], false, Some(&b"v"[..]));
    let cases: Vec<(&str, Vec<Given>, &str)> = vec![
        (
            "an empty segment",
            vec![(vec![vec![]], false, None)],
            "key segment length or depth out of range",
        ),
        (
            "a segment too long",
            vec![(vec![vec![1; MAX_SEGMENT + 1]], false, None)],
            "key segment length or depth out of range",
        ),
        (
            "a list at the deepest level",
            at(MAX_DEPTH - 1, 1, (under(MAX_DEPTH, 1, None), true, None)),
            "a list at the deepest level",
        ),
        (
            "a chunked value at the deepest level",
            at(
                MAX_DEPTH - 1,
                1,
                (under(MAX_DEPTH - 1, 1, Some(vec![1])), false, Some(&BIG)),
            ),
            "a chunked value at the deepest level",
        ),
        (
            "a list with a value",
            vec![(vec![vec![1]], true, Some(&b"v"[..]))],
            "a list key cannot carry a value",
        ),
        (
            "a value no page holds under its path",
            at(
                14,
                MAX_SEGMENT,
                (
                    under(14, MAX_SEGMENT, Some(long.clone())),
                    false,
                    Some(&SMALL),
                ),
            ),
            "a key path too long to paginate",
        ),
        (
            "pieces no page holds under their path",
            at(
                14,
                MAX_SEGMENT,
                (
                    under(14, MAX_SEGMENT, Some(long.clone())),
                    false,
                    Some(&BIG),
                ),
            ),
            "a key path too long to paginate",
        ),
        (
            "an empty path",
            vec![(vec![], false, None)],
            "a node without a key path",
        ),
        (
            "a key given twice",
            vec![leaf(&[1]), leaf(&[1])],
            "key out of order or duplicated at 1",
        ),
        (
            "keys out of order",
            vec![leaf(&[2]), leaf(&[1])],
            "key out of order or duplicated at 1",
        ),
        (
            "a list left and entered again",
            vec![
                (vec![vec![1]], true, None),
                (vec![vec![1], vec![5]], false, None),
                leaf(&[2]),
                (vec![vec![1], vec![6]], false, None),
            ],
            "key out of order or duplicated at 1/6",
        ),
        (
            "a child of a value",
            vec![leaf(&[1]), (vec![vec![1], vec![2]], false, None)],
            "a node whose parent was not given as a list",
        ),
        (
            "a child of a chunked value",
            vec![
                (vec![vec![1]], false, Some(&BIG)),
                (vec![vec![1], vec![0]], false, None),
            ],
            "a node whose parent was not given as a list",
        ),
        (
            "a child of a missing node",
            vec![
                (vec![vec![1]], true, None),
                (vec![vec![1], vec![2], vec![3]], false, None),
            ],
            "a node whose parent was not given as a list",
        ),
        // A path past the limits is refused before it is compared or shown,
        // and an error shows a bounded part of a path.
        (
            "a segment of a mebibyte out of order",
            vec![leaf(&[0xff]), (vec![vec![1; 1 << 20]], false, None)],
            "key segment length or depth out of range",
        ),
        (
            "a path deeper than a file holds",
            vec![leaf(&[0xff]), (vec![vec![1]; MAX_DEPTH + 1], false, None)],
            "key segment length or depth out of range",
        ),
        (
            "a longest segment out of order",
            vec![leaf(&[0xff]), (vec![vec![1; MAX_SEGMENT]], false, None)],
            "key out of order or duplicated at x01010101010101010101010101010101…",
        ),
    ];
    for (name, nodes, reason) in cases {
        let mut writer = NodeWriter::new(Cursor::new(Vec::new()), &superblock).unwrap();
        let (last, before) = nodes.split_last().unwrap();
        for (path, push, value) in before {
            writer
                .node(path, *push, *value)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        let refused = writer
            .node(&last.0, last.1, last.2)
            .unwrap_err()
            .to_string();
        assert!(refused.ends_with(reason), "{name}: {refused}");
        assert!(refused.len() < 200, "{name}: {} bytes", refused.len());
        let stopped = "the writer stopped at an earlier error";
        let next = writer.node(&[[0xff]], false, None).unwrap_err();
        assert!(next.to_string().ends_with(stopped), "{name}: {next}");
        assert!(writer.finish().unwrap_err().to_string().ends_with(stopped));
    }

    // Page 0 is one page, and the target holds nothing yet.
    assert!(NodeWriter::new(Cursor::new(Vec::new()), &superblock[..100]).is_err());
    let refused = NodeWriter::new(Cursor::new(vec![0]), &superblock)
        .err()
        .unwrap();
    assert!(refused.to_string().ends_with("the target is not empty"));
}
