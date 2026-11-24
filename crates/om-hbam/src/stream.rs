//! A file's key tree read node by node, in key order, from a file the
//! reader can seek in, without loading the file or building the tree
//! (`docs/server-storage.md` 6.10, the store's import).
//!
//! [`NodeStream`] reads page 0, then the level-0 leaf chain one page at a
//! time (file-format.md 2.2), and decodes each page's chunk stream with the
//! rules [`crate::HbamFile`] reads it with: the same superblock, page and
//! chain checks (`crate::container`) and the same chunk decoding and key
//! path rule (`crate::slot`).
//!
//! **What a node is.** A node is a key of the tree [`crate::KeyTree`]
//! builds from the file's slots. The first push or leaf slot at a path
//! makes the key; a later slot at the same path lands on it, as a page's
//! re-push of its open path does (file-format.md 3.2), and so does a leaf
//! given to a key that was pushed, or a push of a key that was a leaf
//! (which no file FileMaker wrote has, hbam-writer.md 2.2). A key's kind
//! and value are those of all the slots at its path: it is a list if one of
//! them is a push, and its value is the one a slot gives it. Since slots
//! come in key order, a later slot can only reach a key on the tree's
//! rightmost path, which is all the stream keeps (with each key's kind).
//! A key is returned once, when the next slot makes another key or the
//! chain ends, so the slots that land on it straight after it, such as a
//! page's re-push, are counted. The keys come out in the order of a
//! depth-first walk of that tree, a key before its children
//! ([`crate::Node::walk`]), with the same path, kind and value. A value the
//! format splits into pieces (file-format.md 3.6) comes out as the format
//! holds it: a list whose children are the header and the pieces. Empty
//! lists come out like any other key.
//!
//! **Memory** is one page for page 0, one for the page being read, a copy
//! of at most one value (a value whose key is returned only after its page
//! was left), and the rightmost path: at most
//! [`crate::slot::MAX_DEPTH`] segments of up to 255 bytes. Pages are at
//! most [`MAX_PAGE_SIZE`] bytes.
//!
//! **What it refuses.** Everything [`crate::HbamFile`] refuses: a missing
//! magic, an unusable page size or a file of partial pages when it opens; a
//! broken leaf chain, a bad page header, an undecodable chunk, a key out of
//! order or a key given two values while it reads (an encrypted file fails
//! there, as in `HbamFile`). A check the two readers share gives the same
//! error kind in both. A file with more than one fault can fail on a
//! different one here: the stream stops at the first fault in reading
//! order, and its own checks below can come before one `HbamFile` reports.
//! A chain that loops is stopped by its back links
//! (`container::ChainLinks`) and reported as a broken chain, "prev link
//! does not match", where `HbamFile` says "cycle". It also refuses five
//! things `HbamFile` reads:
//!
//! - a page larger than [`MAX_PAGE_SIZE`] ([`Error::BadPageSize`]), which
//!   it would have to hold;
//! - a page that does not begin by pushing again the path the page before
//!   it left open ([`Error::BrokenChain`]; hbam-writer.md 2.1). Each page's
//!   stream starts at the root, so without that the pages do not join into
//!   one stream, and a pop on the later page would seem to close the root
//!   list while lists of the earlier page are still open;
//! - a value given to a key after its children ([`Error::KeyOrder`]),
//!   which a stream cannot return before them;
//! - a leaf chain that ends before its stream closes the root list
//!   ([`Error::BrokenChain`]), which is what a file cut short looks like;
//! - a leaf chain that goes on after its stream closed the root list
//!   ([`Error::BrokenChain`]).
//!
//! Every file FileMaker wrote pushes the open path again at each page start
//! (hbam-writer.md 2.1) and closes the root list with the last byte of its
//! tail page (file-format.md 3.6), and no key of it has a value and
//! children (hbam-writer.md 2.2).
//!
//! The stream returns keys before it finds an error further on; a reader
//! that stores what it is given treats an error at any point as the file's
//! refusal.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;

use crate::container::{
    find_leaf_head, page_size_of, ChainLinks, Page, PageHeader, Superblock, HEADER_LEN,
    PAGE_HEADER_LEN,
};
use crate::error::{Error, Result};
use crate::slot::{decode_chunk, path_string, step, ChunkKind, KeySegment, Step};

/// The largest page a [`NodeStream`] reads: a body of 65,535 bytes, the
/// most FileMaker's page body length `mPageDataMax`, a u16, can say
/// (file-format.md 2.4). Every file FileMaker wrote has 4,096-byte pages.
pub const MAX_PAGE_SIZE: u32 = u16::MAX as u32 + PAGE_HEADER_LEN as u32;

/// Why a page that does not push again the path left open before it is
/// refused.
const UNJOINED: &str = "the page does not push again the path left open before it";

/// A node's kind: what [`crate::Node`]'s `push` and `value` say together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// A key with a value (possibly empty) that opens no list.
    Value,
    /// A key that opens a list (possibly empty) and has no value.
    List,
    /// A list with a value. No file FileMaker wrote has one
    /// (hbam-writer.md 2.2) and the writer refuses it; it is read as
    /// [`crate::KeyTree`] reads it.
    Both,
}

/// One node of a [`NodeStream`], borrowed from it until the next is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamNode<'s> {
    /// The key's path from the root, its own segment last.
    pub path: &'s [Vec<u8>],
    /// Whether the key opens a list, as [`crate::Node::push`].
    pub push: bool,
    /// The key's value, as [`crate::Node::value`]: `Some` for a leaf
    /// (possibly empty) or a list with a value.
    pub value: Option<&'s [u8]>,
}

impl StreamNode<'_> {
    pub fn kind(&self) -> NodeKind {
        match (self.push, self.value.is_some()) {
            (true, true) => NodeKind::Both,
            (true, false) => NodeKind::List,
            // A key is made by a push or a leaf, and a leaf always gives a
            // value, so a key that is no list has one.
            (false, _) => NodeKind::Value,
        }
    }

    /// The key's own segment, the last of its path.
    pub fn segment(&self) -> &[u8] {
        self.path.last().map_or(&[], Vec::as_slice)
    }
}

/// The nodes of a file's key tree in key order, read from `R` one page at a
/// time (see the module documentation).
///
/// ```no_run
/// let mut nodes = om_hbam::stream::NodeStream::open("file.fmp12")?;
/// while let Some(node) = nodes.next_node()? {
///     println!("{:02x?} {:?}", node.path, node.kind());
/// }
/// # Ok::<(), om_hbam::Error>(())
/// ```
pub struct NodeStream<R> {
    source: R,
    /// The source's offset, when it is known, so pages read in file order
    /// need no seek.
    position: Option<u64>,
    page_size: u32,
    page_count: u32,
    superblock: Vec<u8>,
    /// The bytes of page `in_buffer`.
    buffer: Vec<u8>,
    in_buffer: Option<u32>,
    /// The leaf page being read, once the chain's head is found.
    current: Option<Current>,
    links: ChainLinks,
    /// The offset of the next chunk in the current page's body.
    offset: usize,
    /// How deep the current page's key path is: its segments are the first
    /// `depth` of the spine's (each page re-pushes its path from the root,
    /// file-format.md 3.2, and every push lands on or makes a spine key).
    depth: usize,
    /// Whether the current page's stream closed the root list.
    root_closed: bool,
    /// How many segments of the path the page before left open the current
    /// page has still to push again, before any other slot or pop
    /// (hbam-writer.md 2.1).
    repush: usize,
    spine: Spine,
    /// After the end or an error: nothing more is read.
    done: bool,
}

/// The leaf page in the buffer.
#[derive(Clone, Copy, Debug)]
struct Current {
    number: u32,
    next: u32,
    /// The length of its chunk stream.
    used: usize,
}

/// Where the value of the key not yet returned is.
#[derive(Clone, Debug, Default)]
enum ValueAt {
    #[default]
    None,
    /// In the current page's body.
    Body(Range<usize>),
    /// In [`Spine::held`], copied when its page was left.
    Held,
}

/// A key the stream returns: the first `len` segments of the spine, and
/// its kind and value.
#[derive(Clone, Debug)]
struct Out {
    len: usize,
    push: bool,
    value: ValueAt,
}

/// The kind of a key on the spine.
#[derive(Clone, Copy, Debug, Default)]
struct Kind {
    push: bool,
    value: bool,
}

/// The tree's rightmost path: the keys later slots can still land on, as
/// `crate::tree::SlotOrder` keeps it. Every key on it has been returned but
/// the last, which is pending until the next key is made.
#[derive(Debug, Default)]
struct Spine {
    /// Each level's segment, the first `len` of them the path. Kept past
    /// `len` so their allocations are reused.
    segments: Vec<Vec<u8>>,
    kinds: Vec<Kind>,
    len: usize,
    /// Whether the last key has not been returned yet.
    pending: bool,
    value: ValueAt,
    held: Vec<u8>,
}

impl Spine {
    /// Place a slot whose path is the first `depth` segments and `key`
    /// (`depth <= len`), as [`crate::KeyTree::insert`] inserts it: it lands
    /// on the key at that path when the spine has it, else it makes a key
    /// there, which ends the keys below `depth`. Returns the pending key
    /// instead when the slot makes a key: the caller returns that first and
    /// places the slot again.
    fn place(
        &mut self,
        depth: usize,
        push: bool,
        key: &[u8],
        value: Option<Range<usize>>,
    ) -> Result<Option<Out>> {
        debug_assert!(depth <= self.len);
        let lands = if depth < self.len {
            match self.segments[depth].as_slice().cmp(key) {
                std::cmp::Ordering::Equal => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Greater => return Err(self.out_of_order(depth, key)),
            }
        } else {
            false
        };
        if !lands {
            if let Some(out) = self.take_pending() {
                return Ok(Some(out));
            }
            if self.segments.len() == depth {
                self.segments.push(Vec::new());
                self.kinds.push(Kind::default());
            }
            let segment = &mut self.segments[depth];
            segment.clear();
            segment.extend_from_slice(key);
            self.kinds[depth] = Kind {
                push,
                value: value.is_some(),
            };
            self.len = depth + 1;
            self.pending = true;
            self.value = value.map_or(ValueAt::None, ValueAt::Body);
            return Ok(None);
        }
        // The key was returned already unless it is the pending one, and
        // then it has children: a change to its kind would come after them.
        let pending = self.pending && depth + 1 == self.len;
        let kind = self.kinds[depth];
        if let Some(value) = value {
            if kind.value || !pending {
                return Err(self.out_of_order(depth, key));
            }
            self.kinds[depth].value = true;
            self.value = ValueAt::Body(value);
        }
        if push && !kind.push {
            if !pending {
                return Err(self.out_of_order(depth, key));
            }
            self.kinds[depth].push = true;
        }
        Ok(None)
    }

    /// The pending key, now returned.
    fn take_pending(&mut self) -> Option<Out> {
        if !std::mem::replace(&mut self.pending, false) {
            return None;
        }
        Some(Out {
            len: self.len,
            push: self.kinds[self.len - 1].push,
            value: std::mem::take(&mut self.value),
        })
    }

    /// Copy the pending key's value out of `body`, the page being left.
    fn hold(&mut self, body: &[u8]) {
        if let ValueAt::Body(range) = &self.value {
            self.held.clear();
            self.held.extend_from_slice(&body[range.clone()]);
            self.value = ValueAt::Held;
        }
    }

    fn out_of_order(&self, depth: usize, key: &[u8]) -> Error {
        let mut path: Vec<KeySegment<'_>> = self.segments[..depth]
            .iter()
            .map(|s| KeySegment(s))
            .collect();
        path.push(KeySegment(key));
        Error::KeyOrder {
            path: path_string(&path),
        }
    }
}

impl NodeStream<File> {
    /// Open the file at `path`, as [`NodeStream::new`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::new(File::open(path)?)
    }
}

impl<R: Read + Seek> NodeStream<R> {
    /// Read page 0 of `source` and check it as [`crate::HbamFile::from_bytes`]
    /// does, and that its pages are at most [`MAX_PAGE_SIZE`] bytes. Reads
    /// whole pages at their offsets, so a `File` needs no buffering.
    pub fn new(mut source: R) -> Result<Self> {
        let len = source.seek(SeekFrom::End(0))?;
        source.seek(SeekFrom::Start(0))?;
        let mut head = [0u8; HEADER_LEN];
        let head_len = len.min(HEADER_LEN as u64) as usize;
        source.read_exact(&mut head[..head_len])?;
        let page_size = page_size_of(&head[..head_len], len)?;
        if page_size > MAX_PAGE_SIZE {
            return Err(Error::BadPageSize(page_size));
        }
        // A usable page size holds the header, so `head` is whole.
        let mut superblock = vec![0; page_size as usize];
        superblock[..HEADER_LEN].copy_from_slice(&head);
        source.read_exact(&mut superblock[HEADER_LEN..])?;
        // Page numbers are 32 bits (file-format.md 2.2): a longer file has
        // pages no link can name, and is not read as if it ended there.
        let page_count =
            u32::try_from(len / u64::from(page_size)).map_err(|_| Error::BrokenChain {
                page: 0,
                reason: "the file has more pages than a page number can name",
            })?;
        Ok(NodeStream {
            source,
            position: Some(u64::from(page_size)),
            page_size,
            page_count,
            superblock,
            buffer: vec![0; page_size as usize],
            in_buffer: None,
            current: None,
            links: ChainLinks::default(),
            offset: 0,
            depth: 0,
            root_closed: false,
            repush: 0,
            spine: Spine::default(),
            done: false,
        })
    }

    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    /// Page count, page 0 included (from the file's length).
    pub fn page_count(&self) -> u32 {
        self.page_count
    }

    /// Page 0, as read when the stream was made.
    pub fn superblock(&self) -> Superblock<'_> {
        Superblock {
            raw: &self.superblock,
        }
    }

    /// The next node in key order, or `None` after the last. After an
    /// error, nothing more is read and this returns `None`.
    pub fn next_node(&mut self) -> Result<Option<StreamNode<'_>>> {
        if self.done {
            return Ok(None);
        }
        let out = match self.advance() {
            Ok(Some(out)) => out,
            Ok(None) => {
                self.done = true;
                return Ok(None);
            }
            Err(e) => {
                self.done = true;
                return Err(e);
            }
        };
        let value = match &out.value {
            ValueAt::None => None,
            ValueAt::Body(range) => Some(&self.buffer[PAGE_HEADER_LEN..][range.clone()]),
            ValueAt::Held => Some(self.spine.held.as_slice()),
        };
        Ok(Some(StreamNode {
            path: &self.spine.segments[..out.len],
            push: out.push,
            value,
        }))
    }

    /// Read on to the next key to return.
    fn advance(&mut self) -> Result<Option<Out>> {
        loop {
            let Some(page) = self.current else {
                self.next_page()?;
                continue;
            };
            if self.offset < page.used {
                let body = &self.buffer[PAGE_HEADER_LEN..PAGE_HEADER_LEN + page.used];
                let chunk = decode_chunk(body, self.offset)?;
                let end = self.offset + chunk.raw.len();
                let step = step(chunk.kind, self.depth, self.root_closed, self.offset)?;
                if self.repush > 0 && step != Step::Begin {
                    // The first `depth` segments of the spine are the path
                    // pushed again so far; the next one is due.
                    let again = step == Step::Push
                        && self.depth < self.spine.len
                        && self.spine.segments[self.depth] == chunk.key;
                    if !again {
                        return Err(Error::BrokenChain {
                            page: page.number,
                            reason: UNJOINED,
                        });
                    }
                    self.repush -= 1;
                }
                match step {
                    Step::Begin => {}
                    Step::Pop => self.depth -= 1,
                    Step::CloseRoot => self.root_closed = true,
                    Step::Push | Step::Leaf => {
                        let push = chunk.kind == ChunkKind::Push;
                        // A chunk's value is its last bytes.
                        let value = chunk.slot_value().map(|v| end - v.len()..end);
                        if let Some(out) = self.spine.place(self.depth, push, chunk.key, value)? {
                            return Ok(Some(out));
                        }
                        if push {
                            self.depth += 1;
                        }
                    }
                }
                self.offset = end;
                continue;
            }
            let broken = |reason| Error::BrokenChain {
                page: page.number,
                reason,
            };
            if self.repush > 0 {
                return Err(broken(UNJOINED));
            }
            if page.next == 0 {
                if !self.root_closed {
                    return Err(broken("the leaf chain ends before the root list closes"));
                }
                return Ok(self.spine.take_pending());
            }
            if self.root_closed {
                return Err(broken("the root list closes before the leaf chain ends"));
            }
            self.spine
                .hold(&self.buffer[PAGE_HEADER_LEN..PAGE_HEADER_LEN + page.used]);
            self.next_page()?;
        }
    }

    /// Move to the chain's next page (its head, the first time); the
    /// caller knows there is one.
    fn next_page(&mut self) -> Result<()> {
        let links = self.links;
        let page_count = self.page_count;
        let number = links
            .next_page(page_count, || {
                find_leaf_head(page_count, |n| self.load(n).map(|(header, _)| header))
            })?
            .ok_or(Error::BrokenChain {
                page: 0,
                reason: "the leaf chain has no page",
            })?;
        let (header, used) = if self.in_buffer == Some(number) {
            let page = Page::parse(number, &self.buffer)?;
            (page.header, page.used.len())
        } else {
            self.load(number)?
        };
        self.links.follow(number, &header)?;
        self.current = Some(Current {
            number,
            next: header.next,
            used,
        });
        self.offset = 0;
        // The path open at the end of the page before, which this page's
        // stream must push again.
        self.repush = self.depth;
        self.depth = 0;
        self.root_closed = false;
        Ok(())
    }

    /// Read page `number` into the buffer: its header and the length of its
    /// chunk stream.
    fn load(&mut self, number: u32) -> Result<(PageHeader, usize)> {
        let at = u64::from(number) * u64::from(self.page_size);
        let seek = self.position != Some(at);
        // Until the read succeeds, neither is known.
        self.in_buffer = None;
        self.position = None;
        if seek {
            self.source.seek(SeekFrom::Start(at))?;
        }
        self.source.read_exact(&mut self.buffer)?;
        self.position = Some(at + u64::from(self.page_size));
        self.in_buffer = Some(number);
        let page = Page::parse(number, &self.buffer)?;
        Ok((page.header, page.used.len()))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::container::FREE_LEVEL;
    use crate::writer::rebuild;
    use crate::{new_superblock, HbamFile, OwnedNode};

    /// Each node: its path, whether it is a push, and its value.
    type Flat = Vec<(Vec<Vec<u8>>, bool, Option<Vec<u8>>)>;

    /// The depth-first walk of the tree `HbamFile` builds, root left out.
    fn walked(bytes: &[u8]) -> Result<Flat> {
        let file = HbamFile::from_bytes(bytes.to_vec())?;
        let tree = file.key_tree()?;
        let mut out = Vec::new();
        tree.root.walk(&mut |path, node| {
            if !path.is_empty() {
                let path = path.iter().map(|s| s.0.to_vec()).collect();
                out.push((path, node.push, node.value.map(<[u8]>::to_vec)));
            }
        });
        Ok(out)
    }

    fn streamed(bytes: &[u8]) -> Result<Flat> {
        let mut nodes = NodeStream::new(Cursor::new(bytes))?;
        let mut out = Vec::new();
        while let Some(node) = nodes.next_node()? {
            let kind = match (node.push, node.value.is_some()) {
                (true, true) => NodeKind::Both,
                (true, false) => NodeKind::List,
                _ => NodeKind::Value,
            };
            assert_eq!(node.kind(), kind);
            out.push((
                node.path.to_vec(),
                node.push,
                node.value.map(<[u8]>::to_vec),
            ));
        }
        Ok(out)
    }

    /// The stream is the walk, node by node; returns the node count.
    fn assert_streams_as_walked(bytes: &[u8]) -> usize {
        let walked = walked(bytes).unwrap();
        let streamed = streamed(bytes).unwrap();
        assert_eq!(streamed.len(), walked.len());
        for (i, (s, w)) in streamed.iter().zip(&walked).enumerate() {
            assert_eq!(s, w, "node {i}");
        }
        walked.len()
    }

    fn written(root: &OwnedNode) -> Vec<u8> {
        rebuild(root, &new_superblock("Stream").unwrap()).unwrap()
    }

    /// A file of 4,096-byte pages: page 0, an index root that neither
    /// reader reads, and leaf pages 2, 3, ... with these bodies, chained in
    /// that order.
    fn file_of(bodies: &[&[u8]]) -> Vec<u8> {
        let mut out = new_superblock("Stream").unwrap();
        let mut page = |level: u16, prev: u32, next: u32, body: &[u8]| {
            let header = PageHeader {
                log_seq_num: 0,
                prev,
                next,
                level,
                free_space: (4096 - PAGE_HEADER_LEN - body.len()) as u16,
                mod_count: 1,
            };
            let start = out.len();
            out.extend(header.to_bytes());
            out.extend(body);
            out.resize(start + 4096, 0);
        };
        let last = bodies.len() as u32 + 1;
        page(1, 0, last, &[]);
        for (i, body) in bodies.iter().enumerate() {
            let number = i as u32 + 2;
            let next = if number == last { 0 } else { number + 1 };
            page(0, if i == 0 { 0 } else { number - 1 }, next, body);
        }
        out
    }

    /// Set page `page`'s links.
    fn link(file: &mut [u8], page: usize, prev: u32, next: u32) {
        file[page * 4096 + 4..page * 4096 + 8].copy_from_slice(&prev.to_be_bytes());
        file[page * 4096 + 8..page * 4096 + 12].copy_from_slice(&next.to_be_bytes());
    }

    /// A small xorshift generator, so generated trees are the same on
    /// every run.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }

        fn bytes(&mut self, len: usize) -> Vec<u8> {
            (0..len).map(|_| self.below(256) as u8).collect()
        }

        /// Between `min` and `min + spread - 1` bytes.
        fn value(&mut self, min: usize, spread: u64) -> Vec<u8> {
            let len = min + self.below(spread) as usize;
            self.bytes(len)
        }
    }

    /// Up to `width` children at `depth` (1 for the root's), in key order:
    /// values from empty to over a page, values split into pieces, empty
    /// lists and lists, segments of 1 to 219 bytes, within what the writer
    /// writes (a list or a split value at depth 15 at most).
    fn children(rng: &mut Rng, depth: usize, width: u64) -> Vec<OwnedNode> {
        let mut out: Vec<OwnedNode> = Vec::new();
        for _ in 0..rng.below(width) + 1 {
            let len = match rng.below(10) {
                0 => 219,
                1 => 1 + rng.below(219) as usize,
                _ => 1 + rng.below(3) as usize,
            };
            let segment = rng.bytes(len);
            let deep = depth >= 15;
            let node = match rng.below(8) {
                0 if !deep => OwnedNode::list(segment, Vec::new()),
                1 | 2 if !deep => {
                    let width = (width / 2).max(2);
                    OwnedNode::list(segment, children(rng, depth + 1, width))
                }
                3 if !deep => OwnedNode::leaf(segment, rng.value(1001, 4000)),
                4 => OwnedNode::leaf(segment, Vec::new()),
                _ => OwnedNode::leaf(segment, rng.value(0, 300)),
            };
            out.push(node);
        }
        out.sort_by(|a, b| a.segment.cmp(&b.segment));
        out.dedup_by(|a, b| a.segment == b.segment);
        out
    }

    /// For trees `rebuild` writes, the stream gives exactly the nodes of
    /// the depth-first walk of the tree `HbamFile` builds from the same
    /// bytes, kinds included: lists re-pushed at page starts, values split
    /// into pieces (with two-byte piece keys past piece 127), empty lists
    /// and values, long segments and 16-segment paths.
    #[test]
    fn written_trees_stream_as_walked() {
        let leaf = |segment: &[u8], len: usize| OwnedNode::leaf(segment, vec![0x5a; len]);
        let records: Vec<OwnedNode> = (1..=3000u32)
            .map(|id| {
                let fields = vec![
                    leaf(&[1], (id % 40) as usize),
                    OwnedNode::list(vec![2], Vec::new()),
                    leaf(&[3], if id % 97 == 0 { 2500 } else { 8 }),
                ];
                OwnedNode::list(crate::serial::encode(u64::from(id)), fields)
            })
            .collect();
        // Under `3`, fourteen lists of 219-byte segments: every page under
        // them re-pushes 3,096 bytes. Their leaves are 16 segments deep.
        let deep = (1..14u8).rev().fold(
            OwnedNode::list(vec![16; 219], (0..60u8).map(|k| leaf(&[k], 100)).collect()),
            |child, d| OwnedNode::list(vec![d; 219], vec![child]),
        );
        let shallow_deep = (1..15u8).rev().fold(leaf(&[16], 4), |child, d| {
            OwnedNode::list(vec![d], vec![child])
        });
        let root = OwnedNode::list(
            Vec::new(),
            vec![
                OwnedNode::list(vec![1], Vec::new()),
                leaf(&[2], 0),
                OwnedNode::list(vec![3], vec![deep]),
                OwnedNode::list(vec![4], vec![shallow_deep]),
                leaf(&[5], 128_500),
                OwnedNode::list(vec![0x80, 0x01], records),
                OwnedNode::list(vec![0xff; 219], vec![OwnedNode::list(vec![1], Vec::new())]),
            ],
        );
        let bytes = written(&root);
        assert!(bytes.len() / 4096 > 50, "{} pages", bytes.len() / 4096);
        assert_streams_as_walked(&bytes);

        // An empty tree, and generated ones.
        assert_eq!(assert_streams_as_walked(&written(&OwnedNode::default())), 0);
        for seed in 1..=6u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let root = OwnedNode::list(Vec::new(), children(&mut rng, 1, 40));
            assert!(assert_streams_as_walked(&written(&root)) > 0);
        }
    }

    /// A list with a value, which the writer refuses, and the slot
    /// sequences that merge into one key, built by hand: they stream as
    /// the tree reads them. A value given after the key's children, which
    /// the tree takes, is refused.
    #[test]
    fn keys_made_of_several_slots_stream_as_walked() {
        // A push with a one-byte value, holding one leaf.
        let valued_push: &[u8] = &[0x80, 0x21, 0x05, 0x07, 0x80, 0x01, 0x01, 0xaa, 0x40, 0x40];
        // A leaf, then a push of the same key, holding one leaf.
        let leaf_then_push: &[u8] = &[
            0x80, 0x01, 0x05, 0xaa, 0x20, 0x05, 0x80, 0x01, 0x01, 0xbb, 0x40, 0x40,
        ];
        for body in [valued_push, leaf_then_push] {
            let bytes = file_of(&[body]);
            assert_streams_as_walked(&bytes);
            let mut nodes = NodeStream::new(Cursor::new(&bytes)).unwrap();
            assert_eq!(nodes.next_node().unwrap().unwrap().kind(), NodeKind::Both);
        }
        // A page ending with a leaf whose key the next page pushes. The
        // value is held across the page change.
        let bytes = file_of(&[
            &[0x80, 0x20, 0x03, 0x80, 0x01, 0x05, 0xaa],
            &[
                0x20, 0x03, 0x20, 0x05, 0x80, 0x01, 0x01, 0xbb, 0x40, 0x40, 0x40,
            ],
        ]);
        assert_streams_as_walked(&bytes);
        // A leaf last on its page, returned while the next page is read.
        let bytes = file_of(&[
            &[0x80, 0x20, 0x03, 0x80, 0x06, 0x05, 0x03, 1, 2, 3],
            &[0x20, 0x03, 0x01, 0x06, 0xbb, 0x40, 0x40],
        ]);
        assert_streams_as_walked(&bytes);

        // The value after the key's children.
        let bytes = file_of(&[&[
            0x80, 0x20, 0x05, 0x80, 0x01, 0x01, 0xbb, 0x40, 0x01, 0x05, 0xaa, 0x40,
        ]]);
        assert!(walked(&bytes).is_ok());
        assert!(matches!(streamed(&bytes), Err(Error::KeyOrder { .. })));
        // Keys out of order and a key given two values fail in both.
        for body in [
            &[0x80, 0x01, 0x05, 0xaa, 0x01, 0x04, 0xaa, 0x40][..],
            &[0x80, 0x01, 0x05, 0xaa, 0x01, 0x05, 0xaa, 0x40],
        ] {
            let bytes = file_of(&[body]);
            assert!(matches!(walked(&bytes), Err(Error::KeyOrder { .. })));
            assert!(matches!(streamed(&bytes), Err(Error::KeyOrder { .. })));
        }
    }

    /// Chains that loop, leave the file or are cut short, and broken
    /// headers end the stream with an error, the one `HbamFile` gives
    /// where it refuses the file too.
    #[test]
    fn hostile_files_end_with_an_error() {
        let two = file_of(&[
            &[0x80, 0x20, 0x03, 0x80, 0x01, 0x05, 0xaa],
            &[0x20, 0x03, 0x40, 0x40],
        ]);
        assert_streams_as_walked(&two);
        let kind = |r: &Result<Flat>| std::mem::discriminant(r.as_ref().unwrap_err());
        let both_refuse = |bytes: &[u8]| {
            let (walked, streamed) = (walked(bytes), streamed(bytes));
            assert!(
                walked.is_err() && streamed.is_err(),
                "{walked:?} {streamed:?}"
            );
            assert_eq!(kind(&walked), kind(&streamed), "{walked:?} {streamed:?}");
            streamed.unwrap_err()
        };
        // A loop back to the head, a page linking to itself, a loop past
        // the head with every other back link right.
        let mut looped = two.clone();
        link(&mut looped, 3, 2, 2);
        assert!(matches!(both_refuse(&looped), Error::BrokenChain { .. }));
        let mut itself = two.clone();
        link(&mut itself, 2, 0, 2);
        assert!(matches!(both_refuse(&itself), Error::BrokenChain { .. }));
        let mut three = file_of(&[&[0x80, 0x01, 0x05, 0xaa], &[0x01, 0x06, 0xaa], &[0x40]]);
        link(&mut three, 4, 3, 3);
        assert!(matches!(both_refuse(&three), Error::BrokenChain { .. }));
        // A link out of the file, to the root, and to a free page.
        let mut out = two.clone();
        link(&mut out, 2, 0, 99);
        assert!(matches!(
            both_refuse(&out),
            Error::BrokenChain { page: 99, .. }
        ));
        let mut to_root = two.clone();
        link(&mut to_root, 2, 0, 1);
        assert!(matches!(both_refuse(&to_root), Error::BrokenChain { .. }));
        let mut to_free = two.clone();
        to_free[3 * 4096 + 12..3 * 4096 + 14].copy_from_slice(&FREE_LEVEL.to_be_bytes());
        assert!(matches!(both_refuse(&to_free), Error::BrokenChain { .. }));
        // Cut short: by a page, so the head's link leaves the file, and
        // inside a page.
        assert!(matches!(
            both_refuse(&two[..3 * 4096]),
            Error::BrokenChain { .. }
        ));
        assert!(matches!(
            both_refuse(&two[..3 * 4096 + 100]),
            Error::PartialPage { .. }
        ));
        // A page before the head whose free space is larger than its body.
        let mut bad = two.clone();
        bad[4096 + 14..4096 + 16].copy_from_slice(&5000u16.to_be_bytes());
        assert!(matches!(both_refuse(&bad), Error::BadPageHeader { .. }));
        // A chunk running past its page, and an undecodable one.
        for body in [&[0x80, 0x07, 0x05, 0x10, 0x00][..], &[0x80, 0x41]] {
            both_refuse(&file_of(&[body]));
        }
        // No HBAM7 magic; a file too short for its page size, which made
        // `HbamFile` index past its end; an unusable page size.
        both_refuse(&[0u8; 8192]);
        both_refuse(&two[..0x14]);
        let mut small = two.clone();
        small[0x14..0x18].copy_from_slice(&100u32.to_be_bytes());
        assert!(matches!(both_refuse(&small), Error::BadPageSize(100)));

        // What the stream refuses and `HbamFile` reads: a page that does
        // not push again the list the page before left open (its pop would
        // seem to close the root with that list still open), or pushes
        // another key or holds a leaf in its place; a chain that ends
        // before the root list closes (the tail page lost), one that goes
        // on after it closed, and pages larger than it holds.
        let open: &[u8] = &[0x80, 0x20, 0x05, 0x80, 0x01, 0x01, 0xaa];
        for tail in [
            &[0x40][..],
            &[0x20, 0x06, 0x40, 0x40],
            &[0x01, 0x05, 0xbb, 0x40],
            &[],
        ] {
            let unjoined = file_of(&[open, tail]);
            assert!(matches!(
                streamed(&unjoined),
                Err(Error::BrokenChain { page: 3, .. })
            ));
        }
        assert!(walked(&file_of(&[open, &[0x40]])).is_ok());
        assert_streams_as_walked(&file_of(&[open, &[0x20, 0x05, 0x40, 0x40]]));
        let mut cut = two[..3 * 4096].to_vec();
        link(&mut cut, 2, 0, 0);
        assert!(walked(&cut).is_ok());
        assert!(matches!(streamed(&cut), Err(Error::BrokenChain { .. })));
        let after = file_of(&[&[0x80, 0x01, 0x05, 0xaa, 0x40], &[0x01, 0x06, 0xbb]]);
        assert!(walked(&after).is_ok());
        assert!(matches!(streamed(&after), Err(Error::BrokenChain { .. })));
        let mut huge = vec![0u8; 1 << 17];
        huge[..4096].copy_from_slice(&new_superblock("Stream").unwrap());
        huge[0x14..0x18].copy_from_slice(&(1u32 << 17).to_be_bytes());
        assert!(HbamFile::from_bytes(huge.clone()).is_ok());
        assert!(matches!(
            NodeStream::new(Cursor::new(&huge)),
            Err(Error::BadPageSize(_))
        ));

        // After an error the stream gives nothing more.
        let mut nodes = NodeStream::new(Cursor::new(&looped)).unwrap();
        assert!(nodes.next_node().unwrap().is_some());
        assert!(nodes.next_node().is_err());
        assert!(nodes.next_node().unwrap().is_none());
    }
}
