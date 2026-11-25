//! The HBAM7 container: superblock, pages, and the level-0 leaf chain
//! (file-format.md sections 1 and 2).

use std::path::Path;

use crate::error::{Error, Result};
use crate::slot::{Chunks, KeySegment, Slot, SlotReader};
use crate::tree::KeyTree;

/// Container magic at superblock offset 0x0f.
pub const MAGIC: &[u8; 5] = b"HBAM7";
const MAGIC_OFFSET: usize = 0x0f;
const PAGE_SIZE_OFFSET: usize = 0x14;
/// Fixed format signature at superblock 0x04..0x0f, identical in every sample.
pub const SIGNATURE: [u8; 11] = [
    0x00, 0x02, 0x00, 0x01, 0x00, 0x05, 0x00, 0x02, 0x00, 0x02, 0xc0,
];
const SIGNATURE_OFFSET: usize = 0x04;
/// Version record: u16 big-endian length at 0x208, then the record.
const VERSION_LEN_OFFSET: usize = 0x208;
/// The clean-close byte and its "closed cleanly" value (hbam-writer.md 5.1).
const CLEAN_CLOSE_OFFSET: usize = 0x800;
const CLEAN_CLOSE: u8 = 0xac;
/// File-identity fence region (`EAFI:RC` ... `CR:IFAE`).
const IDENTITY_FENCE: std::ops::Range<usize> = 0xc00..0x1000;
/// Bytes 0x00..0x04 and the version block 0x200..0x228 (the u32 before
/// the record, its length and the record), the same in every file
/// FileMaker wrote (file-format.md 1.2).
const LEADING: [u8; 4] = [0x00, 0x01, 0x00, 0x00];
const VERSION_BLOCK_OFFSET: usize = 0x200;
const VERSION_BLOCK: [u8; 40] =
    *b"\0\0\0\0\0\0\0\x01\0\x1e\0\x0e\0HBAM2125JAN11\xc1\x02\x48\x08Pro 12.0\xc0\xc0";
/// The fence around the name the file was created under: the opening
/// text, the name, `/` and CR, spaces, then the closing text ending the
/// page.
const FENCE_OPEN: &[u8] = b"EAFI:RC\rR,";
const FENCE_NAME_END: &[u8] = b"/\r";
const FENCE_CLOSE: &[u8] = b"\rCR:IFAE";
/// The longest creation name the fence holds, in UTF-8 bytes.
pub const MAX_CREATION_NAME: usize = IDENTITY_FENCE.end
    - IDENTITY_FENCE.start
    - FENCE_OPEN.len()
    - FENCE_NAME_END.len()
    - FENCE_CLOSE.len();

/// Size of the on-disk page header `HPgHdr`.
pub const PAGE_HEADER_LEN: usize = 0x14;
/// `PhysLevel` value marking a free page.
pub const FREE_LEVEL: u16 = 0xffff;
/// The B-tree root page in every observed file (file-format.md 1.3).
pub const ROOT_PAGE: u32 = 1;

/// The 20-byte page header `HPgHdr`, all fields big-endian (file-format.md 2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageHeader {
    /// Redo-log sequence number; 0 in every cleanly closed sample.
    pub log_seq_num: u32,
    /// Previous page at this level; 0 = chain head.
    pub prev: u32,
    /// Next page at this level; 0 = chain tail.
    pub next: u32,
    /// B-tree level: 0 = leaf, >0 = index, [`FREE_LEVEL`] = free.
    pub level: u16,
    /// Unused bytes at the end of the page body.
    pub free_space: u16,
    /// Page modification counter.
    pub mod_count: u32,
}

impl PageHeader {
    pub fn parse(b: &[u8; PAGE_HEADER_LEN]) -> Self {
        let u32_at = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        PageHeader {
            log_seq_num: u32_at(0),
            prev: u32_at(4),
            next: u32_at(8),
            level: u16::from_be_bytes([b[0x0c], b[0x0d]]),
            free_space: u16::from_be_bytes([b[0x0e], b[0x0f]]),
            mod_count: u32_at(0x10),
        }
    }

    pub fn to_bytes(&self) -> [u8; PAGE_HEADER_LEN] {
        let mut out = [0u8; PAGE_HEADER_LEN];
        out[0..4].copy_from_slice(&self.log_seq_num.to_be_bytes());
        out[4..8].copy_from_slice(&self.prev.to_be_bytes());
        out[8..12].copy_from_slice(&self.next.to_be_bytes());
        out[12..14].copy_from_slice(&self.level.to_be_bytes());
        out[14..16].copy_from_slice(&self.free_space.to_be_bytes());
        out[16..20].copy_from_slice(&self.mod_count.to_be_bytes());
        out
    }

    pub fn is_free(&self) -> bool {
        self.level == FREE_LEVEL
    }

    /// Whether this is the head of the leaf chain: a level-0 page with no
    /// previous page (file-format.md 1.3). The first such page is the head.
    pub(crate) fn is_leaf_head(&self) -> bool {
        self.level == 0 && self.prev == 0
    }
}

/// One page of the file. `header + used + slack` is the page byte for byte.
#[derive(Clone, Copy, Debug)]
pub struct Page<'a> {
    pub number: u32,
    pub header: PageHeader,
    /// The whole page, header included.
    pub raw: &'a [u8],
    /// The chunk stream: `mFileData[..pageDataMax - FreeSpace]`.
    pub used: &'a [u8],
    /// Body bytes after the chunk stream (unused; preserved verbatim).
    pub slack: &'a [u8],
}

impl<'a> Page<'a> {
    /// Page `number` from its bytes `raw` (header included): its header, and
    /// its body split into the chunk stream and the slack after it. Both
    /// readers parse pages here ([`HbamFile::page`],
    /// [`crate::stream::NodeStream`]).
    pub(crate) fn parse(number: u32, raw: &'a [u8]) -> Result<Self> {
        let header = PageHeader::parse(raw.first_chunk::<PAGE_HEADER_LEN>().ok_or(
            Error::BadPageHeader {
                page: number,
                reason: "page shorter than its header",
            },
        )?);
        let body = &raw[PAGE_HEADER_LEN..];
        let used_len = if header.is_free() {
            0
        } else {
            body.len()
                .checked_sub(usize::from(header.free_space))
                .ok_or(Error::BadPageHeader {
                    page: number,
                    reason: "free space exceeds page body",
                })?
        };
        Ok(Page {
            number,
            header,
            raw,
            used: &body[..used_len],
            slack: &body[used_len..],
        })
    }

    /// The raw chunks of this page (leaf or index).
    pub fn chunks(&self) -> Chunks<'a> {
        Chunks::new(self.used)
    }

    /// The key/value slots of this page. Every page re-establishes its key
    /// path from the root (file-format.md 3.2), so the stack starts empty.
    pub fn slots(&self) -> SlotReader<'a> {
        SlotReader::new(self.used)
    }
}

/// Page 0 (file-format.md 1.2).
#[derive(Clone, Copy, Debug)]
pub struct Superblock<'a> {
    /// The whole superblock page, preserved for round-trip.
    pub raw: &'a [u8],
}

/// Page 0 of a new file created under `name` (file-format.md 1.2): the
/// fixed bytes every FileMaker file has, the page size, the version
/// record and the identity fence holding `name`; closed cleanly with SCN
/// 0, which [`crate::writer::rebuild`] raises. Refused for a name the fence
/// cannot hold, or holding `/` or a line break, which would end it early.
pub fn new_superblock(name: &str) -> Result<Vec<u8>> {
    if name.is_empty() || name.len() > MAX_CREATION_NAME || name.contains(['/', '\r', '\n']) {
        return Err(Error::Write(
            "a file's creation name must be 1 to 1004 UTF-8 bytes without / or line breaks",
        ));
    }
    let mut page = vec![0u8; IDENTITY_FENCE.end];
    page[..LEADING.len()].copy_from_slice(&LEADING);
    page[SIGNATURE_OFFSET..SIGNATURE_OFFSET + SIGNATURE.len()].copy_from_slice(&SIGNATURE);
    page[MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()].copy_from_slice(MAGIC);
    page[PAGE_SIZE_OFFSET..PAGE_SIZE_OFFSET + 4].copy_from_slice(&0x1000u32.to_be_bytes());
    page[VERSION_BLOCK_OFFSET..VERSION_BLOCK_OFFSET + VERSION_BLOCK.len()]
        .copy_from_slice(&VERSION_BLOCK);
    page[CLEAN_CLOSE_OFFSET] = CLEAN_CLOSE;
    let fence = &mut page[IDENTITY_FENCE];
    fence.fill(b' ');
    let mut at = 0;
    for part in [FENCE_OPEN, name.as_bytes(), FENCE_NAME_END] {
        fence[at..at + part.len()].copy_from_slice(part);
        at += part.len();
    }
    let close = fence.len() - FENCE_CLOSE.len();
    fence[close..].copy_from_slice(FENCE_CLOSE);
    Ok(page)
}

impl<'a> Superblock<'a> {
    pub fn page_size(&self) -> u32 {
        let o = PAGE_SIZE_OFFSET;
        u32::from_be_bytes([
            self.raw[o],
            self.raw[o + 1],
            self.raw[o + 2],
            self.raw[o + 3],
        ])
    }

    /// Whether 0x04..0x0f holds the fixed format signature seen in every sample.
    pub fn signature_matches(&self) -> bool {
        self.raw[SIGNATURE_OFFSET..SIGNATURE_OFFSET + SIGNATURE.len()] == SIGNATURE
    }

    /// The length-delimited version record at 0x20a (structure opaque; it
    /// carries the engine build tag and format version strings).
    pub fn version_record(&self) -> Option<&'a [u8]> {
        let len = usize::from(u16::from_be_bytes([
            self.raw[VERSION_LEN_OFFSET],
            self.raw[VERSION_LEN_OFFSET + 1],
        ]));
        self.raw
            .get(VERSION_LEN_OFFSET + 2..VERSION_LEN_OFFSET + 2 + len)
    }

    /// Printable ASCII runs of 4+ bytes in the version record, e.g.
    /// `["HBAM2125JAN11", "Pro 12.0"]`.
    pub fn version_strings(&self) -> Vec<String> {
        ascii_runs(self.version_record().unwrap_or_default(), 4)
    }

    /// The file-identity fence (0xc00..0x1000), when present in this page.
    pub fn identity_fence(&self) -> Option<&'a [u8]> {
        self.raw.get(IDENTITY_FENCE)
    }
}

fn ascii_runs(bytes: &[u8], min: usize) -> Vec<String> {
    bytes
        .split(|b| !(0x20..0x7f).contains(b))
        .filter(|run| run.len() >= min)
        .map(|run| String::from_utf8_lossy(run).into_owned())
        .collect()
}

/// The bytes of page 0 [`page_size_of`] reads: the magic and the page size.
pub(crate) const HEADER_LEN: usize = PAGE_SIZE_OFFSET + 4;

/// The page size of a file `len` bytes long whose first bytes are `head`
/// (at least [`HEADER_LEN`] of them, or the whole file when it is shorter),
/// checked as both readers check it ([`HbamFile::from_bytes`],
/// [`crate::stream::NodeStream`]): the `HBAM7` magic, a page size that
/// holds a page header and page 0's version record, and a file of whole
/// pages (file-format.md 1.1, 1.2). A file too short to hold the page size
/// has an unusable one, 0.
pub(crate) fn page_size_of(head: &[u8], len: u64) -> Result<u32> {
    if head.get(MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()) != Some(&MAGIC[..]) {
        return Err(Error::NotHbam);
    }
    let page_size = head
        .get(PAGE_SIZE_OFFSET..HEADER_LEN)
        .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let min = (VERSION_LEN_OFFSET + 2).max(PAGE_HEADER_LEN + 1);
    if (page_size as usize) < min {
        return Err(Error::BadPageSize(page_size));
    }
    if len % u64::from(page_size) != 0 {
        return Err(Error::PartialPage {
            len: usize::try_from(len).unwrap_or(usize::MAX),
            page_size,
        });
    }
    Ok(page_size)
}

/// An opened HBAM7 file. Holds the bytes unchanged; every view borrows them,
/// so [`HbamFile::as_bytes`] always round-trips the original file.
#[derive(Clone, Debug)]
pub struct HbamFile {
    data: Vec<u8>,
    page_size: u32,
}

impl HbamFile {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_bytes(std::fs::read(path)?)
    }

    /// Validate the superblock and page geometry. Encrypted (EAR) files are not
    /// supported: their reserved page tail makes chunk decoding fail.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        let page_size = page_size_of(&data, data.len() as u64)?;
        Ok(HbamFile { data, page_size })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    /// Page count, superblock included (implicit from the file length).
    pub fn page_count(&self) -> u32 {
        (self.data.len() / self.page_size as usize) as u32
    }

    /// Usable body bytes per page (`mPageDataMax`, unencrypted).
    pub fn page_data_max(&self) -> usize {
        self.page_size as usize - PAGE_HEADER_LEN
    }

    pub fn superblock(&self) -> Superblock<'_> {
        Superblock {
            raw: &self.data[..self.page_size as usize],
        }
    }

    /// B-tree page `number` (1-based; page 0 is the superblock).
    pub fn page(&self, number: u32) -> Result<Page<'_>> {
        if number == 0 || number >= self.page_count() {
            return Err(Error::BadPageHeader {
                page: number,
                reason: "page number out of range",
            });
        }
        let size = self.page_size as usize;
        Page::parse(
            number,
            &self.data[number as usize * size..(number as usize + 1) * size],
        )
    }

    /// The first level-0 page (`PrevPage == 0`); page 2 in every sample.
    pub fn leaf_head(&self) -> Result<u32> {
        find_leaf_head(self.page_count(), |n| Ok(self.page(n)?.header))
    }

    /// Walk the leaf chain from its head, validating links (file-format.md 2.2, 5).
    pub fn leaf_pages(&self) -> LeafPages<'_> {
        LeafPages {
            file: self,
            links: ChainLinks::default(),
            seen: vec![false; self.page_count() as usize],
            done: false,
        }
    }

    /// Every key/value slot of the file in key order (the complete read path,
    /// file-format.md 2.3).
    pub fn slots(&self) -> FileSlots<'_> {
        FileSlots {
            pages: self.leaf_pages(),
            current: None,
        }
    }

    /// The whole key space as a tree.
    pub fn key_tree(&self) -> Result<KeyTree<'_>> {
        KeyTree::from_slots(self.slots())
    }

    /// The subtree of keys whose numeric path starts with `prefix`
    /// (e.g. `[table_id, 5]` for one table's records).
    pub fn key_tree_under(&self, prefix: &[u64]) -> Result<KeyTree<'_>> {
        KeyTree::from_slots_where(self.slots(), |path| path_has_prefix(path, prefix))
    }

    /// The node at a path of raw segment bytes, with its whole subtree, or
    /// `None` when the file has no such key. Serve it on the wire with
    /// [`crate::Node::to_wire`].
    pub fn subtree(&self, path: &[&[u8]]) -> Result<Option<crate::Node<'_>>> {
        let tree = KeyTree::from_slots_where(self.slots(), |slot_path| {
            slot_path
                .iter()
                .zip(path)
                .all(|(segment, want)| segment.0 == *want)
        })?;
        Ok(tree.node(path).cloned())
    }
}

/// Whether `path` starts with the numeric segments `prefix` (or is an
/// ancestor of it, so the pushes leading to the prefix are kept).
pub fn path_has_prefix(path: &[KeySegment<'_>], prefix: &[u64]) -> bool {
    path.iter()
        .zip(prefix)
        .all(|(seg, want)| seg.as_uint() == Some(*want))
}

/// The head of the leaf chain in a file of `page_count` pages: the first
/// page, from page 1 on, whose header (read by `header`) is a level-0 page
/// with no previous page (file-format.md 1.3). Both readers find it here.
pub(crate) fn find_leaf_head(
    page_count: u32,
    mut header: impl FnMut(u32) -> Result<PageHeader>,
) -> Result<u32> {
    for n in 1..page_count {
        if header(n)?.is_leaf_head() {
            return Ok(n);
        }
    }
    Err(Error::BrokenChain {
        page: 0,
        reason: "no level-0 chain head",
    })
}

/// The walk of the level-0 leaf chain from its head, checked one page at a
/// time (file-format.md 2.2, 5): each page must be in the file, a leaf, and
/// link back to the page before it. Both readers walk the chain with it
/// ([`LeafPages`], [`crate::stream::NodeStream`]).
///
/// The back links alone keep a walk from reading a page twice, so it needs
/// no record of the pages it has read. The head must link back to 0 and
/// every later page to the page read before it. Take the first page the
/// walk reaches a second time. If it was the head, its back link is 0, but
/// must now be the page read before it, which is never 0. Otherwise its
/// back link is the page read before its first visit, and must now be the
/// page read before this one: that page would be reached twice, earlier.
/// Either way the check fails, so a walk ends, with an error or at the
/// tail, within the file's page count.
///
/// That holds for a source that gives the same bytes each time a page is
/// read. One that changes under the walk (a file another process is
/// writing) could pass every back link and still lead it round, so the
/// walk also counts its pages and stops when it has read as many as the
/// file holds.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChainLinks {
    /// The page the walk reads next: `None` before the head, `Some(0)`
    /// after the tail.
    next: Option<u32>,
    /// The page read last (0 before the head).
    prev: u32,
    /// How many pages the walk has read.
    read: u32,
}

impl ChainLinks {
    /// The page to read next, in a file of `page_count` pages, or `None`
    /// after the tail. `head` finds the head, the first time.
    pub(crate) fn next_page(
        &self,
        page_count: u32,
        head: impl FnOnce() -> Result<u32>,
    ) -> Result<Option<u32>> {
        let number = match self.next {
            Some(0) => return Ok(None),
            Some(n) => n,
            None => head()?,
        };
        if number >= page_count {
            return Err(Error::BrokenChain {
                page: number,
                reason: "link out of range",
            });
        }
        // Page 0 is no leaf, so a chain has fewer pages than the file.
        if self.read >= page_count.saturating_sub(1) {
            return Err(Error::BrokenChain {
                page: number,
                reason: "the chain has more pages than the file",
            });
        }
        Ok(Some(number))
    }

    /// Check page `number`, the one [`Self::next_page`] named, with its
    /// `header`, as the next page of the chain, and follow its link.
    pub(crate) fn follow(&mut self, number: u32, header: &PageHeader) -> Result<()> {
        let broken = |reason| Error::BrokenChain {
            page: number,
            reason,
        };
        if header.level != 0 {
            return Err(broken("non-leaf page in leaf chain"));
        }
        if header.prev != self.prev {
            return Err(broken("prev link does not match"));
        }
        self.prev = number;
        self.next = Some(header.next);
        self.read = self.read.saturating_add(1);
        Ok(())
    }
}

/// Iterator over the level-0 leaf chain.
pub struct LeafPages<'a> {
    file: &'a HbamFile,
    links: ChainLinks,
    /// The pages read, so a cycle is reported as one (the back links
    /// would stop it anyway, [`ChainLinks`]).
    seen: Vec<bool>,
    done: bool,
}

impl<'a> LeafPages<'a> {
    fn step(&mut self) -> Result<Option<Page<'a>>> {
        let file = self.file;
        let Some(number) = self
            .links
            .next_page(file.page_count(), || file.leaf_head())?
        else {
            return Ok(None);
        };
        let broken = |reason| Error::BrokenChain {
            page: number,
            reason,
        };
        let slot = self
            .seen
            .get_mut(number as usize)
            .ok_or(broken("link out of range"))?;
        if std::mem::replace(slot, true) {
            return Err(broken("cycle"));
        }
        let page = file.page(number)?;
        self.links.follow(number, &page.header)?;
        Ok(Some(page))
    }
}

impl<'a> Iterator for LeafPages<'a> {
    type Item = Result<Page<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let item = self.step().transpose();
        if !matches!(item, Some(Ok(_))) {
            self.done = true;
        }
        item
    }
}

/// All slots of the leaf chain, page by page.
pub struct FileSlots<'a> {
    pages: LeafPages<'a>,
    current: Option<SlotReader<'a>>,
}

impl<'a> Iterator for FileSlots<'a> {
    type Item = Result<Slot<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(reader) = &mut self.current {
                if let Some(slot) = reader.next() {
                    return Some(slot);
                }
            }
            match self.pages.next()? {
                Ok(page) => self.current = Some(page.slots()),
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source that changes under the walk can satisfy every back link
    /// and still lead it round: here pages 2 and 3 each name the other as
    /// both neighbours on every read after the head's first. The walk ends
    /// once it has read as many pages as the file has leaves to give.
    #[test]
    fn a_chain_walk_ends_within_the_files_pages_whatever_the_headers_say() {
        let header = |prev, next| PageHeader {
            log_seq_num: 0,
            prev,
            next,
            level: 0,
            free_space: 0,
            mod_count: 0,
        };
        let page_count = 4;
        let mut links = ChainLinks::default();
        let mut read = 0;
        let ended = loop {
            let number = match links.next_page(page_count, || Ok(2)) {
                Ok(Some(number)) => number,
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            };
            let other = if number == 2 { 3 } else { 2 };
            let prev = if read == 0 { 0 } else { other };
            links.follow(number, &header(prev, other)).unwrap();
            read += 1;
            assert!(read < 100, "the walk does not end");
        };
        assert!(matches!(ended, Err(Error::BrokenChain { .. })), "{ended:?}");
        assert_eq!(read, page_count - 1);
    }

    #[test]
    fn header_round_trip() {
        let bytes = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 78, 0, 0, 0x04, 0x9d, 0, 0, 0x1d, 0x66,
        ];
        let h = PageHeader::parse(&bytes);
        assert_eq!(
            (h.prev, h.next, h.level, h.free_space, h.mod_count),
            (0, 78, 0, 1181, 0x1d66)
        );
        assert_eq!(h.to_bytes(), bytes);
    }

    #[test]
    fn rejects_non_hbam() {
        assert!(matches!(
            HbamFile::from_bytes(vec![0; 8192]),
            Err(Error::NotHbam)
        ));
        let mut data = vec![0u8; 4096 + 7];
        data[MAGIC_OFFSET..MAGIC_OFFSET + 5].copy_from_slice(MAGIC);
        data[PAGE_SIZE_OFFSET..PAGE_SIZE_OFFSET + 4].copy_from_slice(&4096u32.to_be_bytes());
        assert!(matches!(
            HbamFile::from_bytes(data),
            Err(Error::PartialPage { .. })
        ));
    }
}
