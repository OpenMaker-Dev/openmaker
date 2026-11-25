//! [`NodeWriter`]: the bulk writer of hbam-writer.md 7.2, given a key tree
//! one node at a time.
//!
//! **Input.** Page 0 as the file had it, then the tree's nodes in the order
//! of a depth-first walk ([`crate::Node::walk`]: a key before its children,
//! siblings in key order), each as its path from the root (its own segment
//! last), whether it opens a list, and its value: the fields of
//! [`crate::Node`] and of a node read from a file a node at a time, so a
//! file read that way can be written straight back. A value over 1,000
//! bytes is cut into pieces here (2.3); a value the format already holds
//! as pieces is a list of the header and the pieces, and is written as it
//! is given.
//!
//! **Output.** The file [`super::rebuild`] writes for the same tree and
//! page 0, byte for byte, since `rebuild` runs on this writer: the leaves
//! from page 2 as the nodes arrive, each index level after the level below
//! it, the top one as the root at page 1, then page 0 with the clean-close
//! flag set and the final change number (5.1). The target must be empty,
//! and the file is complete only once [`NodeWriter::finish`] returns.
//! Flushing it to the device (`File::sync_all`) and putting it in place
//! are the caller's to do; the writer calls only [`Write::flush`].
//!
//! **Why the target is read back.** The index pages follow the leaves, and
//! the numbers of an index level's pages are known only once the level
//! below it is complete. Rather than hold an entry for every page of the
//! level below until then, the writer reads that level back from the
//! target one page at a time and takes each page's first key from its
//! bytes (sections 1 and 3.2). That is why the target must be readable as
//! well as writable and seekable: a `File` opened for reading and writing,
//! or a `Cursor<Vec<u8>>`.
//!
//! What is read back is checked against what was written. As it writes a
//! level's pages the writer folds each page's first key and number into a
//! checksum, it folds the same again from the bytes it reads back, and
//! `finish` fails when the two differ. A target that returns bytes other
//! than it was given, as a faulty device or cache could, so fails the file
//! instead of getting a wrong index. The checksum is no defence against
//! a target built to defeat it.
//!
//! **Memory.** Page 0, the leaf page being filled, the open key path (at
//! most 15 lists of up to 219 bytes) and the last node's segment; while
//! the index is built, one page read back, the index page being filled,
//! the separator last added to it and three 64-bit checksums. A value is
//! borrowed while it is written, not copied. Nothing grows with the file.
//!
//! **Refusals.** What [`super::rebuild`] refuses, with the same reasons:
//! a node [`super::unwritable`] reports, refused when it arrives; at the
//! end, an index that would need more than 16 levels; and, at the page
//! that runs out of them, change numbers the file has no room for. Also
//! refused: a node that does not come after the one before it in key order
//! ([`Error::KeyOrder`]), and a node whose parent was not given as a list
//! (missing, or given as a value). A path deeper than 16 or with a segment
//! over 219 bytes is refused before anything else is done with it, and an
//! error shows at most the first 16 bytes of each segment of a path. After
//! an error every call fails, and the target holds part of a file, for the
//! caller to discard.

use std::cmp::Ordering;
use std::io::{Read, Seek, SeekFrom, Write};

use super::{
    key_len, leaf_len, push_len, refusal_of, BODY, MAX_DEPTH, MAX_INLINE_VALUE, MAX_SEGMENT,
    PAGE_SIZE,
};
use crate::container::{PageHeader, PAGE_HEADER_LEN};
use crate::error::{Error, Result};
use crate::serial;
use crate::slot::{
    decode_chunk, encode_chunk_into, minimal_code, ChunkKind, KeySegment, BEGIN, POP,
};

/// Superblock fields (5.1): the clean-close flag and the file SCN.
const CLEAN_CLOSE: usize = 0x800;
const CLEAN: u8 = 0xac;
pub(super) const SCN: usize = 0x802;
/// Index-page flag marking the last slot of a level (3.1).
const LAST: u8 = 0xc0;
/// The root is always page 1 (4.1); the leaves start at page 2.
const ROOT: u32 = 1;
const FIRST_LEAF: u32 = 2;

const STOPPED: &str = "the writer stopped at an earlier error";
const NOT_WRITTEN: &str = "a page read back from the target is not the page written";
const TOO_MANY_PAGES: &str = "the file would need more than 2^32 pages";

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

impl Segment {
    /// The bytes of this segment's key in an index slot.
    fn key_len(&self) -> usize {
        match self {
            Segment::Key(bytes) => key_len(bytes),
            Segment::Begin | Segment::End => 2,
        }
    }

    /// An index slot of this segment (3.1): `code` (the slot's flags and
    /// value class) with the key class, then the key. A fence is class 3,
    /// length 0, then its sentinel.
    fn put_slot(&self, code: u8, out: &mut Vec<u8>) {
        match self {
            Segment::Begin => out.extend([code | 3 << 3, 0x00, 0x01]),
            Segment::End => out.extend([code | 3 << 3, 0x00, 0xff]),
            Segment::Key(bytes) => {
                match bytes.len() {
                    n @ 1..=3 => out.push(code | (n as u8 - 1) << 3),
                    n => out.extend([code | 3 << 3, n as u8]),
                }
                out.extend_from_slice(bytes);
            }
        }
    }
}

/// A running checksum (64-bit FNV-1a) of the first keys and numbers of one
/// level's pages, in page order: all that the level above is built from.
/// Each segment is folded as its kind and, for a key, its length and bytes,
/// and each page as a marker and its number, so no two sequences of keys
/// fold the same bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FirstKeys(u64);

impl Default for FirstKeys {
    fn default() -> Self {
        FirstKeys(0xcbf2_9ce4_8422_2325)
    }
}

impl FirstKeys {
    fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn key(&mut self, segment: &[u8]) {
        self.bytes(&[1]);
        self.bytes(&(segment.len() as u32).to_be_bytes());
        self.bytes(segment);
    }

    fn segment(&mut self, segment: &Segment) {
        match segment {
            Segment::Begin => self.bytes(&[0]),
            Segment::Key(bytes) => self.key(bytes),
            Segment::End => self.bytes(&[2]),
        }
    }

    /// The end of a page's first key, and the page's number.
    fn page(&mut self, number: u32) {
        self.bytes(&[3]);
        self.bytes(&number.to_be_bytes());
    }
}

/// `path` as an error shows it: at most [`MAX_DEPTH`] segments, each cut to
/// its first 16 bytes, so the text has a fixed bound whatever the caller
/// passed.
fn shown<S: AsRef<[u8]>>(path: &[S]) -> String {
    let mut out = String::new();
    for (i, segment) in path.iter().take(MAX_DEPTH).enumerate() {
        let bytes = segment.as_ref();
        if i > 0 {
            out.push('/');
        }
        out.push_str(&KeySegment(&bytes[..bytes.len().min(16)]).to_string());
        if bytes.len() > 16 {
            out.push('…');
        }
    }
    if path.len() > MAX_DEPTH {
        out.push_str("/…");
    }
    out
}

/// Page 0 as a complete file holds it (5.1): the clean-close flag set and
/// the change number `scn`, the last one a page took. `page` is one whole
/// page.
pub(super) fn close_page_zero(page: &mut [u8], scn: u64) {
    page[CLEAN_CLOSE] = CLEAN;
    page[CLEAN_CLOSE + 1] = 0;
    page[SCN..SCN + 8].copy_from_slice(&scn.to_be_bytes());
}

/// A push chunk of `segment`, without its begin.
pub(super) fn put_push(segment: &[u8], out: &mut Vec<u8>) {
    let code = minimal_code(true, segment.len(), None).expect("segment length checked");
    encode_chunk_into(out, code, segment, &[]).expect("push chunk");
}

/// A leaf chunk; an empty value is written with value class 0.
pub(super) fn put_leaf(segment: &[u8], value: &[u8], out: &mut Vec<u8>) {
    let size = (!value.is_empty()).then_some(value.len());
    let code = minimal_code(false, segment.len(), size).expect("sizes checked");
    encode_chunk_into(out, code, segment, value).expect("leaf chunk");
}

/// An item of the leaf stream (section 1) that a page holds whole: a push
/// with its begin is one.
#[derive(Clone, Copy)]
enum Unit<'a> {
    Begin,
    Pop,
    Push(&'a [u8]),
    Leaf(&'a [u8], &'a [u8]),
}

impl Unit<'_> {
    fn len(self) -> usize {
        match self {
            Unit::Begin | Unit::Pop => 1,
            Unit::Push(segment) => push_len(segment) + 1,
            Unit::Leaf(segment, value) => leaf_len(segment, value.len()),
        }
    }

    fn put(self, out: &mut Vec<u8>) {
        match self {
            Unit::Begin => out.push(BEGIN),
            Unit::Pop => out.push(POP),
            Unit::Push(segment) => {
                put_push(segment, out);
                out.push(BEGIN);
            }
            Unit::Leaf(segment, value) => put_leaf(segment, value, out),
        }
    }
}

/// The key path open at the current point of the leaf stream: the segments
/// of the pushes not yet closed (section 1), in one buffer.
#[derive(Default)]
struct OpenPath {
    bytes: Vec<u8>,
    /// Where each segment ends in `bytes`.
    ends: Vec<usize>,
    /// The bytes a page takes to re-push the path (2.1).
    pushed: usize,
}

impl OpenPath {
    fn len(&self) -> usize {
        self.ends.len()
    }

    fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    fn iter(&self) -> impl Iterator<Item = &[u8]> + '_ {
        let starts = std::iter::once(0).chain(self.ends.iter().copied());
        starts
            .zip(&self.ends)
            .map(|(start, &end)| &self.bytes[start..end])
    }

    fn push(&mut self, segment: &[u8]) {
        self.bytes.extend_from_slice(segment);
        self.ends.push(self.bytes.len());
        self.pushed += push_len(segment);
    }

    /// Close the last open segment; nothing when only the root's list is
    /// open.
    fn pop(&mut self) {
        if let Some(end) = self.ends.pop() {
            let start = self.ends.last().copied().unwrap_or(0);
            self.pushed -= push_len(&self.bytes[start..end]);
            self.bytes.truncate(start);
        }
    }
}

/// The target, the position it is at when that is known (so pages written
/// or read in order need no seek), and the file's change number.
struct Out<W> {
    target: W,
    at: Option<u64>,
    scn: u64,
}

impl<W: Read + Write + Seek> Out<W> {
    fn seek(&mut self, offset: u64) -> Result<()> {
        if self.at != Some(offset) {
            self.at = None;
            self.target.seek(SeekFrom::Start(offset))?;
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.seek(offset)?;
        self.at = None;
        self.target.write_all(bytes)?;
        self.at = Some(offset + bytes.len() as u64);
        Ok(())
    }

    fn read_page(&mut self, number: u32, page: &mut [u8]) -> Result<()> {
        let offset = u64::from(number) * PAGE_SIZE as u64;
        self.seek(offset)?;
        self.at = None;
        self.target.read_exact(page)?;
        self.at = Some(offset + page.len() as u64);
        Ok(())
    }

    /// Write `page` (the header's place, then the body) as page `number`
    /// with a fresh change number, zero-filled to a whole page, and empty
    /// it for the next page.
    fn page(
        &mut self,
        number: u32,
        level: u16,
        links: (u32, u32),
        page: &mut Vec<u8>,
    ) -> Result<()> {
        // Each page written takes the next change number (5.2): a stored
        // one without room for them all is refused, not wrapped.
        self.scn = self.scn.checked_add(1).ok_or(Error::Write(
            "the file's change number has no room for the rebuilt pages",
        ))?;
        let header = PageHeader {
            log_seq_num: (self.scn >> 32) as u32,
            prev: links.0,
            next: links.1,
            level,
            free_space: (PAGE_SIZE - page.len()) as u16,
            mod_count: self.scn as u32,
        };
        page[..PAGE_HEADER_LEN].copy_from_slice(&header.to_bytes());
        page.resize(PAGE_SIZE, 0);
        self.write_at(u64::from(number) * PAGE_SIZE as u64, page)?;
        page.clear();
        Ok(())
    }
}

/// Writes a file's key tree, given node by node in key order, as a complete
/// `.fmp12` (see the module documentation).
///
/// ```
/// use std::io::Cursor;
/// use om_hbam::writer::NodeWriter;
///
/// let page0 = om_hbam::new_superblock("Example")?;
/// let mut writer = NodeWriter::new(Cursor::new(Vec::new()), &page0)?;
/// writer.node(&[[0x80, 0x01]], true, None)?;
/// writer.node(&[&[0x80, 0x01][..], &[0x05]], false, Some(b"value"))?;
/// let file = writer.finish()?.into_inner();
/// assert_eq!(file.len(), 3 * 4096);
/// # Ok::<(), om_hbam::Error>(())
/// ```
pub struct NodeWriter<W> {
    out: Out<W>,
    superblock: Vec<u8>,
    open: OpenPath,
    /// Whether the last node given was a value; its segment is then
    /// `last_segment`. Otherwise it was a list, the end of `open`, or
    /// there was none.
    last_value: bool,
    last_segment: Vec<u8>,
    /// The leaf page being filled, the header's place first; empty when
    /// none is.
    page: Vec<u8>,
    /// Leaf pages written.
    leaves: u32,
    /// The first keys of the leaf pages, as written.
    leaf_keys: FirstKeys,
    failed: bool,
}

impl<W: Read + Write + Seek> NodeWriter<W> {
    /// A writer of a file with page 0 `superblock` (the page as the file
    /// had it, 4,096 bytes) to `target`, which must be empty.
    pub fn new(mut target: W, superblock: &[u8]) -> Result<Self> {
        if superblock.len() != PAGE_SIZE {
            return Err(Error::Write("the superblock is not one 4096-byte page"));
        }
        if target.seek(SeekFrom::End(0))? != 0 {
            return Err(Error::Write("the target is not empty"));
        }
        let scn = u64::from_be_bytes(superblock[SCN..SCN + 8].try_into().expect("8 bytes"));
        let mut writer = NodeWriter {
            out: Out {
                target,
                at: Some(0),
                scn,
            },
            superblock: superblock.to_vec(),
            open: OpenPath::default(),
            last_value: false,
            last_segment: Vec::new(),
            page: Vec::with_capacity(PAGE_SIZE),
            leaves: 0,
            leaf_keys: FirstKeys::default(),
            failed: false,
        };
        // The leaf stream opens with the root's list (section 1).
        writer.unit(Unit::Begin)?;
        Ok(writer)
    }

    /// Write the next node of the tree: its `path` from the root, its own
    /// segment last; whether it opens a list (`push`, as
    /// [`crate::Node::push`]: its children follow it); and its `value`
    /// ([`crate::Node::value`]: `None` or empty for no value; a list may
    /// not carry one). A node read from a file a node at a time is passed
    /// as it is read: its path, push flag and value.
    pub fn node<S: AsRef<[u8]>>(
        &mut self,
        path: &[S],
        push: bool,
        value: Option<&[u8]>,
    ) -> Result<()> {
        if self.failed {
            return Err(Error::Write(STOPPED));
        }
        let written = self.write_node(path, push, value);
        self.failed = written.is_err();
        written
    }

    /// Close the tree and write the index, the root and page 0. Returns
    /// the target, flushed with [`Write::flush`] but not to the device.
    pub fn finish(mut self) -> Result<W> {
        if self.failed {
            return Err(Error::Write(STOPPED));
        }
        // Close the lists still open, then the root's.
        while !self.open.is_empty() {
            self.unit(Unit::Pop)?;
        }
        self.unit(Unit::Pop)?;
        self.leaf(true)?;
        self.index()?;
        let mut page = std::mem::take(&mut self.superblock);
        close_page_zero(&mut page, self.out.scn);
        self.out.write_at(0, &page)?;
        self.out.target.flush()?;
        Ok(self.out.target)
    }

    fn write_node<S: AsRef<[u8]>>(
        &mut self,
        path: &[S],
        list: bool,
        value: Option<&[u8]>,
    ) -> Result<()> {
        let Some((segment, parents)) = path.split_last() else {
            return Err(Error::Write("a node without a key path"));
        };
        // Bound what is read of the path before anything else is done
        // with it.
        if path.len() > MAX_DEPTH || path.iter().any(|s| s.as_ref().len() > MAX_SEGMENT) {
            return Err(Error::Write("key segment length or depth out of range"));
        }
        let segment = segment.as_ref();
        // After the node before it in a depth-first walk: below it, or
        // after it and its subtree.
        let last = self.last_value.then_some(self.last_segment.as_slice());
        let before = self.open.iter().chain(last);
        if path.iter().map(AsRef::as_ref).cmp(before) != Ordering::Greater {
            return Err(Error::KeyOrder { path: shown(path) });
        }
        // Its parent is open: the lists open are its ancestors, then lists
        // the walk has left.
        let shared = self
            .open
            .iter()
            .zip(parents)
            .take_while(|(open, parent)| *open == parent.as_ref())
            .count();
        if shared < parents.len() {
            return Err(Error::Write("a node whose parent was not given as a list"));
        }
        while self.open.len() > parents.len() {
            self.unit(Unit::Pop)?;
        }
        let value = value.unwrap_or_default();
        if let Some(reason) = refusal_of(segment, list, value, path.len(), self.open.pushed) {
            return Err(Error::Write(reason));
        }
        self.last_value = !list;
        if list {
            return self.unit(Unit::Push(segment));
        }
        self.last_segment.clear();
        self.last_segment.extend_from_slice(segment);
        if value.len() <= MAX_INLINE_VALUE {
            return self.unit(Unit::Leaf(segment, value));
        }
        // A chunked node (2.3): child `00` = the u32 length, then
        // 1000-byte pieces keyed 1, 2, ... as serial keys (`7f`, then
        // `80 00` for piece 128; containers.md 1.5).
        let total = u32::try_from(value.len()).map_err(|_| Error::Write("value over 4 GiB"))?;
        self.unit(Unit::Push(segment))?;
        self.unit(Unit::Leaf(&[0], &total.to_be_bytes()))?;
        for (i, piece) in value.chunks(MAX_INLINE_VALUE).enumerate() {
            self.unit(Unit::Leaf(&serial::encode(i as u64 + 1), piece))?;
        }
        self.unit(Unit::Pop)
    }

    /// Add `unit` to the leaf stream, cut into pages (2.1): a unit that does
    /// not fit ends the page, and the next page re-pushes the open path.
    fn unit(&mut self, unit: Unit) -> Result<()> {
        let len = unit.len();
        if self.page.is_empty() || self.page.len() + len > PAGE_SIZE {
            if !self.page.is_empty() {
                self.leaf(false)?;
            }
            self.page.resize(PAGE_HEADER_LEN, 0);
            // The page re-pushes the open path, and its first key is that
            // path and what `unit` makes of it (section 1).
            for segment in self.open.iter() {
                put_push(segment, &mut self.page);
                self.leaf_keys.key(segment);
            }
            match unit {
                Unit::Begin => self.leaf_keys.segment(&Segment::Begin),
                Unit::Pop => self.leaf_keys.segment(&Segment::End),
                Unit::Push(segment) => {
                    self.leaf_keys.key(segment);
                    self.leaf_keys.segment(&Segment::Begin);
                }
                Unit::Leaf(segment, _) => self.leaf_keys.key(segment),
            }
            // A number past the last one is refused when the page is
            // written.
            self.leaf_keys.page(FIRST_LEAF.wrapping_add(self.leaves));
            if self.page.len() + len > PAGE_SIZE {
                return Err(Error::Write("a key path too long to paginate"));
            }
        }
        unit.put(&mut self.page);
        match unit {
            Unit::Push(segment) => self.open.push(segment),
            Unit::Pop => self.open.pop(),
            Unit::Begin | Unit::Leaf(..) => {}
        }
        Ok(())
    }

    /// Write the leaf page being filled; `last` when no leaf follows it.
    fn leaf(&mut self, last: bool) -> Result<()> {
        let number = FIRST_LEAF
            .checked_add(self.leaves)
            .ok_or(Error::Write(TOO_MANY_PAGES))?;
        let prev = if self.leaves == 0 { 0 } else { number - 1 };
        let next = if last {
            0
        } else {
            number.checked_add(1).ok_or(Error::Write(TOO_MANY_PAGES))?
        };
        self.out.page(number, 0, (prev, next), &mut self.page)?;
        self.leaves += 1;
        Ok(())
    }

    /// Build the index bottom-up (7.2): each level from the first keys of
    /// the pages of the level below, read back, packed into pages numbered
    /// after every page written so far, until a level is one page: the
    /// root. The first keys read back must fold to the checksum of those
    /// written ([`FirstKeys`]).
    fn index(&mut self) -> Result<()> {
        let too_many = || Error::Write(TOO_MANY_PAGES);
        let end = FIRST_LEAF.checked_add(self.leaves).ok_or_else(too_many)?;
        let mut below = FIRST_LEAF..end;
        let mut below_keys = self.leaf_keys;
        let mut level: u16 = 1;
        let mut read = vec![0; PAGE_SIZE];
        let mut first = Vec::new();
        let mut page = IndexPage::default();
        loop {
            // This level's pages, unless it is only the root.
            let start = below.end;
            let mut written = 0;
            let (mut read_keys, mut level_keys) = (FirstKeys::default(), FirstKeys::default());
            for child in below {
                self.out.read_page(child, &mut read)?;
                first_key(&read, level - 1, &mut first)?;
                first.iter().for_each(|s| read_keys.segment(s));
                read_keys.page(child);
                let opens = if page.is_empty() {
                    true
                } else if page.add(&first, child)? {
                    false
                } else {
                    let number = start.checked_add(written).ok_or_else(too_many)?;
                    let next = number.checked_add(1).ok_or_else(too_many)?;
                    let prev = if written == 0 { 0 } else { number - 1 };
                    page.close();
                    self.out.page(number, level, (prev, next), &mut page.body)?;
                    page.clear();
                    written += 1;
                    true
                };
                if opens {
                    // An empty page takes the entry or refuses it, and the
                    // entry is the page's first key.
                    page.add(&first, child)?;
                    first.iter().for_each(|s| level_keys.segment(s));
                    level_keys.page(start.wrapping_add(written));
                }
            }
            if read_keys != below_keys {
                return Err(Error::Write(NOT_WRITTEN));
            }
            page.close();
            if written == 0 {
                // The root is alone at its level; its NextPage is the last
                // page (4.1).
                return self.out.page(ROOT, level, (0, start - 1), &mut page.body);
            }
            let number = start.checked_add(written).ok_or_else(too_many)?;
            self.out
                .page(number, level, (number - 1, 0), &mut page.body)?;
            page.clear();
            below = start..number.checked_add(1).ok_or_else(too_many)?;
            below_keys = level_keys;
            level += 1;
            if usize::from(level) > MAX_DEPTH {
                return Err(Error::Write("the index grew past 16 levels"));
            }
        }
    }
}

/// The first key of a page of `level` this writer wrote (sections 1 and
/// 3.2), read from its bytes into `out`.
fn first_key(page: &[u8], level: u16, out: &mut Vec<Segment>) -> Result<()> {
    out.clear();
    let header = PageHeader::parse(page[..PAGE_HEADER_LEN].try_into().expect("a whole page"));
    let used = BODY
        .checked_sub(usize::from(header.free_space))
        .filter(|_| header.level == level)
        .ok_or(Error::Write(NOT_WRITTEN))?;
    let body = &page[PAGE_HEADER_LEN..PAGE_HEADER_LEN + used];
    let found = match level {
        0 => leaf_first_key(body, out),
        _ => index_first_key(body, out),
    };
    found.ok_or(Error::Write(NOT_WRITTEN))
}

/// A leaf page's first key: the path it re-pushes (pushes without their
/// begin), then what its first item makes of it (the table of section 1).
fn leaf_first_key(body: &[u8], out: &mut Vec<Segment>) -> Option<()> {
    let mut at = 0;
    loop {
        let chunk = decode_chunk(body, at).ok()?;
        at += chunk.raw.len();
        let last = match chunk.kind {
            ChunkKind::Begin => Segment::Begin,
            ChunkKind::Pop => Segment::End,
            ChunkKind::Leaf => Segment::Key(chunk.key.to_vec()),
            ChunkKind::Push => {
                out.push(Segment::Key(chunk.key.to_vec()));
                if body.get(at) != Some(&BEGIN) {
                    continue;
                }
                Segment::Begin
            }
        };
        out.push(last);
        return Some(());
    }
}

/// An index page's first separator: the first slot at each depth, down to
/// the first terminal slot (3.1).
fn index_first_key(body: &[u8], out: &mut Vec<Segment>) -> Option<()> {
    let mut at = 0;
    loop {
        let code = *body.get(at)?;
        let (segment, len) = match (code >> 3) & 3 {
            class @ 0..=2 => {
                let n = usize::from(class) + 1;
                (Segment::Key(body.get(at + 1..at + 1 + n)?.to_vec()), 1 + n)
            }
            _ => match usize::from(*body.get(at + 1)?) {
                0 => match *body.get(at + 2)? {
                    0x01 => (Segment::Begin, 3),
                    _ => (Segment::End, 3),
                },
                n => (Segment::Key(body.get(at + 2..at + 2 + n)?.to_vec()), 2 + n),
            },
        };
        out.push(segment);
        // A terminal slot, without the interior bit, ends the separator.
        if code & 0x20 == 0 {
            return Some(());
        }
        at += len;
    }
}

/// An index page being filled: its entries (separator, child page) as a
/// prefix trie (3.1), built as they arrive in key order. Sharing is
/// maximal: an entry shares the slots of the segments it has in common
/// with the entry before it and adds a slot for each of its others, the
/// last one terminal. A list's last slot is flagged once a later entry, or
/// the end of the page, closes the list.
#[derive(Default)]
struct IndexPage {
    /// The header's place, then the trie; empty when no entry is.
    body: Vec<u8>,
    /// The separator last added.
    last: Vec<Segment>,
    /// Where the latest slot at each depth of `last` starts in `body`.
    slots: Vec<usize>,
}

impl IndexPage {
    fn is_empty(&self) -> bool {
        self.last.is_empty()
    }

    /// Add `entry` -> `child` if the page has room for it; an empty page
    /// without room refuses it. Entries come in key order, and no
    /// separator is a prefix of another (3.1).
    fn add(&mut self, entry: &[Segment], child: u32) -> Result<bool> {
        let shared = self
            .last
            .iter()
            .zip(entry)
            .take_while(|(a, b)| a == b)
            .count();
        if !self.last.is_empty() {
            match (self.last.get(shared), entry.get(shared)) {
                (Some(last), Some(next)) if last < next => {}
                _ => return Err(Error::Write(NOT_WRITTEN)),
            }
        }
        let added: usize = (shared..entry.len())
            .map(|depth| 1 + entry[depth].key_len() + if depth + 1 == entry.len() { 4 } else { 0 })
            .sum();
        if self.body.len().max(PAGE_HEADER_LEN) + added > PAGE_SIZE {
            if self.last.is_empty() {
                return Err(Error::Write("an index entry too long for a page"));
            }
            return Ok(false);
        }
        if self.body.is_empty() {
            self.body.resize(PAGE_HEADER_LEN, 0);
        }
        // The lists below the shared part end with the last entry's slots.
        for &slot in self.slots.iter().skip(shared + 1) {
            self.body[slot] |= LAST;
        }
        self.slots.truncate(shared);
        for (depth, segment) in entry.iter().enumerate().skip(shared) {
            self.slots.push(self.body.len());
            if depth + 1 < entry.len() {
                segment.put_slot(0x20, &mut self.body);
            } else {
                segment.put_slot(3, &mut self.body);
                self.body.extend(child.to_be_bytes());
            }
        }
        self.last.clear();
        self.last.extend_from_slice(entry);
        Ok(true)
    }

    /// Flag the last slot of every list still open: the page is complete.
    fn close(&mut self) {
        for &slot in &self.slots {
            self.body[slot] |= LAST;
        }
    }

    fn clear(&mut self) {
        self.body.clear();
        self.last.clear();
        self.slots.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page bodies [`IndexPage`] packs `entries` into, as `index`
    /// does.
    fn pack(entries: &[(Vec<Segment>, u32)]) -> Result<Vec<Vec<u8>>> {
        let mut pages = Vec::new();
        let mut page = IndexPage::default();
        for (entry, child) in entries {
            if !page.add(entry, *child)? {
                page.close();
                pages.push(page.body[PAGE_HEADER_LEN..].to_vec());
                page.clear();
                page.add(entry, *child)?;
            }
        }
        page.close();
        pages.push(
            page.body
                .get(PAGE_HEADER_LEN..)
                .unwrap_or_default()
                .to_vec(),
        );
        Ok(pages)
    }

    /// `MAX_INDEX_ENTRY` (`super::super`) on one index page: four entries
    /// without a shared prefix, each costing exactly the bound, fill a
    /// page's body, so a page that refused an entry within the bound held
    /// four; one byte more each, and the fourth does not fit.
    #[test]
    fn four_entries_at_the_index_bound_fill_a_page() {
        use super::super::{index_entry_cost, MAX_INDEX_ENTRY};
        // A leaf's separator of `cost` bytes: four 219-byte segments, the
        // first one the entry's own, then one making up the rest (a
        // segment of `n > 3` bytes takes `n + 2` with its code byte).
        let entry = |first: u8, cost: usize| -> Vec<Segment> {
            let mut path = vec![
                vec![first; 219],
                vec![0x61; 219],
                vec![0x61; 219],
                vec![0x61; 219],
            ];
            path.push(vec![0x62; cost - 4 * 221 - 2 - 4]);
            assert_eq!(index_entry_cost(&path, false), cost);
            path.into_iter().map(Segment::Key).collect()
        };
        assert_eq!(4 * MAX_INDEX_ENTRY, PAGE_SIZE - PAGE_HEADER_LEN);
        for (cost, fit) in [(MAX_INDEX_ENTRY, 4), (MAX_INDEX_ENTRY + 1, 3)] {
            let mut page = IndexPage::default();
            let mut added = 0;
            for first in 0..5u8 {
                if !page
                    .add(&entry(0x41 + first, cost), u32::from(first) + 2)
                    .unwrap()
                {
                    break;
                }
                added += 1;
            }
            assert_eq!(added, fit, "entries of {cost} bytes");
        }
    }

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
        assert_eq!(pack(&entries).unwrap(), std::slice::from_ref(&expected));
        // The first separator reads back from the page.
        let mut first = Vec::new();
        assert!(index_first_key(&expected, &mut first).is_some());
        assert_eq!(first, [Segment::Begin]);
    }

    /// The trie built an entry at a time is the one built from all of a
    /// page's entries at once, and pages break where the greedy packing
    /// of the bulk writer before it streamed broke them; each page's first
    /// separator reads back.
    #[test]
    fn index_pages_match_greedy_packing() {
        // The bulk writer's trie encoder, kept as an oracle.
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
                    if let [(separator, page)] = group {
                        if separator.len() == depth + 1 {
                            separator[depth].put_slot(last | 3, out);
                            out.extend(page.to_be_bytes());
                            continue;
                        }
                    }
                    group[0].0[depth].put_slot(last | 0x20, out);
                    level(group, depth + 1, out);
                }
            }
            let mut out = Vec::new();
            level(entries, 0, &mut out);
            out
        }
        fn greedy(entries: &[(Vec<Segment>, u32)]) -> Vec<Vec<u8>> {
            let mut pages = Vec::new();
            let mut start = 0;
            while start < entries.len() {
                let mut end = start + 1;
                assert!(index_body(&entries[start..end]).len() <= BODY);
                while end < entries.len() && index_body(&entries[start..=end]).len() <= BODY {
                    end += 1;
                }
                pages.push(index_body(&entries[start..end]));
                start = end;
            }
            pages
        }

        for width in [2, 4, crate::writer::MAX_SEGMENT] {
            for nested in [false, true] {
                // Include exact body capacity and its neighbors for the flat
                // two-byte (7 bytes/entry) and four-byte (10 bytes/entry) keys.
                for count in [1, 2, 17, 18, 19, 406, 407, 408, 581, 582, 583, 2_000] {
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
                    let ctx = format!("width={width}, nested={nested}, count={count}");
                    let pages = pack(&entries).unwrap();
                    assert_eq!(pages, greedy(&entries), "{ctx}");
                    let mut at = 0;
                    for page in &pages {
                        let mut first = Vec::new();
                        assert!(index_first_key(page, &mut first).is_some(), "{ctx}");
                        assert_eq!(first, entries[at].0, "{ctx}");
                        // A page ends with its last entry's child.
                        let last = u32::from_be_bytes(page[page.len() - 4..].try_into().unwrap());
                        at = (last - FIRST_LEAF) as usize + 1;
                    }
                }
            }
        }
    }

    #[test]
    fn fences_order_around_real_segments() {
        let k = |b: &[u8]| Segment::Key(b.to_vec());
        assert!(Segment::Begin < k(&[0]));
        assert!(k(&[0xff, 0xff]) < Segment::End);
        assert!(k(&[1]) < k(&[1, 0]));
    }
}
