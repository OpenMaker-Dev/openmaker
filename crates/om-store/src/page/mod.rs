//! Pages of the trees (`docs/server-storage-format.md` section 2): their byte
//! layout, the hash a parent holds for each, the checksum each carries, and
//! the checks a page passes before any of it is trusted.
//!
//! A [`Page`] exists only after those checks ([`Page::verify`]) or when the
//! tree has just built it, so code that holds one holds bytes that are what
//! the page's parent names.

mod memory;
mod store;

pub use memory::{MemEntry, MemStore, Numbering};
pub use store::{EntryPages, PageStore, Release, Root, StoreError, TreeName};

use std::fmt;

use ring::digest;

/// The size of every page.
pub const PAGE_SIZE: usize = 16_384;
/// The tree format version this build writes and reads: the page layouts
/// and the shape rules of `docs/server-storage-format.md` section 2.
pub const FORMAT_VERSION: u8 = 1;
/// The longest encoded path an item may have (`docs/server-storage.md`
/// section 6.11).
pub const MAX_KEY: usize = 65_535;
/// The longest value a node may hold (section 6.11).
pub const MAX_VALUE: usize = 65_535;
/// The highest level an index page may have. Leaves are level 0.
pub const MAX_LEVEL: u8 = 40;

/// A page's place in the page file. Placement is each server's own: no hash
/// covers a page number.
pub type PageNo = u64;

const NUMBER_AT: usize = 8;
/// Where the hashed region starts.
const REGION_AT: usize = 16;
const REGION_HEADER: usize = 24;
const BODY_AT: usize = REGION_AT + REGION_HEADER;
/// What the items or children of one page may take, page numbers included.
pub(crate) const USABLE: usize = PAGE_SIZE - BODY_AT;
/// An item or separator whose in-page form would be larger than this spills.
pub(crate) const SPILL_LIMIT: usize = PAGE_SIZE / 4;
/// How many bytes of a spilled item or separator stay in its page.
pub(crate) const SPILL_INLINE: usize = 1024;
/// The data an overflow page holds.
pub(crate) const OVERFLOW_DATA: usize = USABLE;
const ITEM_HEADER: usize = 5;
const SEPARATOR_HEADER: usize = 2;
const HASH_LEN: usize = 32;
/// A child entry without its separator: hash, stamp and page number.
pub(crate) const CHILD_FOOTPRINT: usize = HASH_LEN + 8 + 8;
/// A reference to an overflow page: its hash in the region and its number
/// in the pointer array.
const REF_FOOTPRINT: usize = HASH_LEN + 8;

/// The kinds of node an item can hold, as its kind byte.
pub(crate) const KIND_LIST: u8 = 1;
pub(crate) const KIND_VALUE: u8 = 2;
pub(crate) const KIND_BOTH: u8 = 3;

/// A SHA-256 hash.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// The SHA-256 hash of `bytes`.
    pub fn of(bytes: &[u8]) -> Hash {
        let digest = digest::digest(&digest::SHA256, bytes);
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        Hash(out)
    }

    /// Lowercase hexadecimal.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", &self.to_hex()[..16])
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// What kind of page a page is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    /// Items: nodes of the key tree, in key order.
    Leaf = 1,
    /// Children and the separators between them.
    Index = 2,
    /// The rest of a spilled item or separator.
    Overflow = 3,
}

/// Why a page cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The store has no page at that number.
    Missing,
    /// The store could not read the page.
    Io(String),
    /// The bytes do not match the page's checksum, or bytes the format
    /// leaves unused are not zero: the page was damaged after it was
    /// written.
    Damaged,
    /// An intact page that names another number: it was written to the wrong
    /// place.
    Misplaced { found: PageNo },
    /// An intact page at its place, but not the version its parent names: a
    /// write that was lost, or a number reused while still referenced.
    Stale { found: Hash },
    /// The page its parent names, whose content breaks the format or does
    /// not fit where its parent puts it.
    Malformed(&'static str),
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::Missing => f.write_str("missing"),
            Fault::Io(e) => write!(f, "unreadable: {e}"),
            Fault::Damaged => f.write_str("damaged (checksum)"),
            Fault::Misplaced { found } => write!(f, "misplaced (it is page {found})"),
            Fault::Stale { found } => write!(f, "stale (its hash is {found})"),
            Fault::Malformed(why) => write!(f, "malformed: {why}"),
        }
    }
}

/// A page that cannot be used, named by its number and the hash its parent
/// holds for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageFault {
    pub page: PageNo,
    pub hash: Hash,
    pub fault: Fault,
}

impl fmt::Display for PageFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "page {} ({}): {}", self.page, self.hash, self.fault)
    }
}

impl std::error::Error for PageFault {}

/// The in-page footprint of an item, page numbers included.
pub(crate) fn item_footprint(key_len: usize, value_len: usize) -> usize {
    let whole = ITEM_HEADER + key_len + value_len;
    if whole <= SPILL_LIMIT {
        whole
    } else {
        ITEM_HEADER + SPILL_INLINE + overflow_pages(key_len + value_len) * REF_FOOTPRINT
    }
}

/// The in-page footprint of a separator (without its child's entry).
pub(crate) fn separator_footprint(len: usize) -> usize {
    let whole = SEPARATOR_HEADER + len;
    if whole <= SPILL_LIMIT {
        whole
    } else {
        SEPARATOR_HEADER + SPILL_INLINE + overflow_pages(len) * REF_FOOTPRINT
    }
}

fn item_spills(key_len: usize, value_len: usize) -> bool {
    ITEM_HEADER + key_len + value_len > SPILL_LIMIT
}

fn separator_spills(len: usize) -> bool {
    SEPARATOR_HEADER + len > SPILL_LIMIT
}

/// How many overflow pages hold the part of `total` bytes that does not stay
/// in the page.
pub(crate) fn overflow_pages(total: usize) -> usize {
    total.saturating_sub(SPILL_INLINE).div_ceil(OVERFLOW_DATA)
}

/// How many bytes of a spilled stream of `total` bytes stay in its page
/// (`None` when the stream does not spill at all).
pub(crate) fn item_inline(key_len: usize, value_len: usize) -> Option<usize> {
    item_spills(key_len, value_len).then_some(SPILL_INLINE)
}

pub(crate) fn separator_inline(len: usize) -> Option<usize> {
    separator_spills(len).then_some(SPILL_INLINE)
}

/// References to overflow pages: their hashes from the region and their
/// numbers from the pointer array, in order.
#[derive(Clone, Copy)]
pub(crate) struct Refs<'p> {
    hashes: &'p [u8],
    numbers: &'p [u8],
}

impl<'p> Refs<'p> {
    pub(crate) fn len(&self) -> usize {
        self.hashes.len() / HASH_LEN
    }

    pub(crate) fn get(&self, i: usize) -> (PageNo, Hash) {
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&self.hashes[i * HASH_LEN..(i + 1) * HASH_LEN]);
        (u64_at(self.numbers, i * 8), Hash(hash))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (PageNo, Hash)> + 'p {
        let refs = *self;
        (0..refs.len()).map(move |i| refs.get(i))
    }
}

/// An item of a leaf page.
#[derive(Clone, Copy)]
pub(crate) struct ItemView<'p> {
    pub kind: u8,
    pub key_len: usize,
    pub value_len: usize,
    /// The key and the value, whole, or the first [`SPILL_INLINE`] bytes of
    /// them when the item spills.
    pub inline: &'p [u8],
    pub refs: Refs<'p>,
}

impl<'p> ItemView<'p> {
    /// The key as far as the page holds it, and whether that is all of it.
    pub(crate) fn key_part(&self) -> (&'p [u8], bool) {
        let n = self.key_len.min(self.inline.len());
        (&self.inline[..n], n == self.key_len)
    }

    /// The value, when it is held whole in the page.
    pub(crate) fn value_inline(&self) -> Option<&'p [u8]> {
        (self.refs.len() == 0).then(|| &self.inline[self.key_len..])
    }
}

/// A child entry of an index page.
#[derive(Clone, Copy)]
pub(crate) struct ChildView<'p> {
    pub number: PageNo,
    pub hash: Hash,
    pub stamp: u64,
    /// The separator below which the child's keys do not go; none for the
    /// first child.
    pub separator: Option<SeparatorView<'p>>,
}

#[derive(Clone, Copy)]
pub(crate) struct SeparatorView<'p> {
    pub len: usize,
    pub inline: &'p [u8],
    pub refs: Refs<'p>,
}

impl<'p> SeparatorView<'p> {
    pub(crate) fn part(&self) -> (&'p [u8], bool) {
        let n = self.len.min(self.inline.len());
        (&self.inline[..n], n == self.len)
    }
}

/// Where an item or child entry starts in the page, and the index of its
/// first number in the pointer array.
#[derive(Clone, Copy)]
struct Slot {
    at: u16,
    first_ref: u16,
}

/// A page that passed its checks, or that the tree has just built.
pub struct Page {
    number: PageNo,
    hash: Hash,
    bytes: Box<[u8]>,
    kind: PageKind,
    level: u8,
    written: u64,
    count: usize,
    region_len: usize,
    refs: usize,
    slots: Vec<Slot>,
}

impl fmt::Debug for Page {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Page")
            .field("number", &self.number)
            .field("hash", &self.hash)
            .field("kind", &self.kind)
            .field("level", &self.level)
            .field("written", &self.written)
            .field("count", &self.count)
            .finish()
    }
}

/// The fields of the page header and the region header, bounds-checked.
struct Header {
    kind: PageKind,
    level: u8,
    count: usize,
    region_len: usize,
    refs: usize,
    written: u64,
}

fn u16_at(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(b)
}

fn hash_at(bytes: &[u8], at: usize) -> Hash {
    let mut h = [0u8; 32];
    h.copy_from_slice(&bytes[at..at + HASH_LEN]);
    Hash(h)
}

impl Header {
    /// Reads the headers, refusing what does not fit the page. Nothing here is
    /// trusted yet: a failure means the bytes are damaged.
    fn read(bytes: &[u8]) -> Option<Header> {
        let r = REGION_AT;
        if bytes[r] != FORMAT_VERSION || bytes[r + 3] != 0 {
            return None;
        }
        let kind = match bytes[r + 1] {
            1 => PageKind::Leaf,
            2 => PageKind::Index,
            3 => PageKind::Overflow,
            _ => return None,
        };
        let region_len = u16_at(bytes, r + 6);
        let refs = u16_at(bytes, r + 8);
        if bytes[r + 10..r + 16].iter().any(|&b| b != 0)
            || region_len < REGION_HEADER
            || REGION_AT + region_len + refs * 8 > PAGE_SIZE
        {
            return None;
        }
        Some(Header {
            kind,
            level: bytes[r + 2],
            count: u16_at(bytes, r + 4),
            region_len,
            refs,
            written: u64_at(bytes, r + 16),
        })
    }

    fn end(&self) -> usize {
        REGION_AT + self.region_len + self.refs * 8
    }
}

fn checksum(hash: &Hash, bytes: &[u8], header: &Header) -> u64 {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(&hash.0);
    ctx.update(&bytes[NUMBER_AT..REGION_AT]);
    ctx.update(&bytes[REGION_AT + header.region_len..header.end()]);
    let digest = ctx.finish();
    let mut b = [0u8; 8];
    b.copy_from_slice(&digest.as_ref()[..8]);
    u64::from_le_bytes(b)
}

impl Page {
    /// Checks the bytes stored at `number` against `expected`, the hash the
    /// page's parent holds, and decodes them. The checks, in order: the
    /// headers fit the page, the checksum matches, unused bytes are zero, the
    /// page names `number`, its hash is `expected`, and its content follows
    /// the format.
    pub fn verify(number: PageNo, expected: &Hash, bytes: Box<[u8]>) -> Result<Page, Fault> {
        if bytes.len() != PAGE_SIZE {
            return Err(Fault::Damaged);
        }
        let header = Header::read(&bytes).ok_or(Fault::Damaged)?;
        let hash = Hash::of(&bytes[REGION_AT..REGION_AT + header.region_len]);
        if u64_at(&bytes, 0) != checksum(&hash, &bytes, &header)
            || bytes[header.end()..].iter().any(|&b| b != 0)
        {
            return Err(Fault::Damaged);
        }
        let found = u64_at(&bytes, NUMBER_AT);
        if found != number {
            return Err(Fault::Misplaced { found });
        }
        if hash != *expected {
            return Err(Fault::Stale { found: hash });
        }
        Page::parse(number, hash, bytes, header)
    }

    /// Decodes a page whose bytes are known to be the ones its parent names.
    /// The content is still checked: a page can be intact and wrong.
    fn parse(number: PageNo, hash: Hash, bytes: Box<[u8]>, h: Header) -> Result<Page, Fault> {
        let end = REGION_AT + h.region_len;
        let mut slots = Vec::new();
        let mut refs = 0usize;
        let mut at = BODY_AT;
        let malformed = Fault::Malformed;
        match h.kind {
            PageKind::Leaf => {
                if h.level != 0 {
                    return Err(malformed("a leaf above level 0"));
                }
                let mut previous: Option<(usize, usize)> = None;
                for _ in 0..h.count {
                    if at + ITEM_HEADER > end {
                        return Err(malformed("an item past the region"));
                    }
                    let kind = bytes[at];
                    let key_len = u16_at(&bytes, at + 1);
                    let value_len = u16_at(&bytes, at + 3);
                    if !(KIND_LIST..=KIND_BOTH).contains(&kind) {
                        return Err(malformed("an item kind"));
                    }
                    if kind == KIND_LIST && value_len != 0 {
                        return Err(malformed("a list with a value"));
                    }
                    let (inline, n) = match item_inline(key_len, value_len) {
                        Some(inline) => (inline, overflow_pages(key_len + value_len)),
                        None => (key_len + value_len, 0),
                    };
                    let item_end = at + ITEM_HEADER + inline + n * HASH_LEN;
                    if item_end > end {
                        return Err(malformed("an item past the region"));
                    }
                    let key_at = at + ITEM_HEADER;
                    let key_part = key_len.min(inline);
                    let complete = key_part == key_len;
                    if !crate::tree::key::well_formed(&bytes[key_at..key_at + key_part], complete) {
                        return Err(malformed("a key that is not an encoded path"));
                    }
                    if let Some((p_at, p_len)) = previous {
                        if !in_order(
                            &bytes[p_at..p_at + p_len],
                            &bytes[key_at..key_at + key_part],
                            complete,
                        ) {
                            return Err(malformed("items out of order"));
                        }
                    }
                    previous = Some((key_at, key_part));
                    slots.push(slot(at, refs)?);
                    refs += n;
                    at = item_end;
                }
            }
            PageKind::Index => {
                if h.level == 0 || h.level > MAX_LEVEL {
                    return Err(malformed("an index level"));
                }
                if h.count == 0 {
                    return Err(malformed("an index page without children"));
                }
                refs = h.count;
                let mut previous: Option<(usize, usize)> = None;
                for i in 0..h.count {
                    let mut entry_end = at + HASH_LEN + 8;
                    if entry_end > end {
                        return Err(malformed("a child past the region"));
                    }
                    if u64_at(&bytes, at + HASH_LEN) > h.written {
                        return Err(malformed("a child written after its parent"));
                    }
                    let first_ref = refs;
                    if i > 0 {
                        if entry_end + SEPARATOR_HEADER > end {
                            return Err(malformed("a separator past the region"));
                        }
                        let len = u16_at(&bytes, entry_end);
                        if len == 0 {
                            return Err(malformed("an empty separator"));
                        }
                        let (inline, n) = match separator_inline(len) {
                            Some(inline) => (inline, overflow_pages(len)),
                            None => (len, 0),
                        };
                        let sep_at = entry_end + SEPARATOR_HEADER;
                        entry_end = sep_at + inline + n * HASH_LEN;
                        if entry_end > end {
                            return Err(malformed("a separator past the region"));
                        }
                        let part = len.min(inline);
                        let complete = part == len;
                        let sep = &bytes[sep_at..sep_at + part];
                        let ok = if complete {
                            crate::tree::key::separator_ok(sep)
                        } else {
                            crate::tree::key::well_formed(sep, false)
                        };
                        if !ok {
                            return Err(malformed("a separator that begins no path"));
                        }
                        if let Some((p_at, p_len)) = previous {
                            if !in_order(
                                &bytes[p_at..p_at + p_len],
                                &bytes[sep_at..sep_at + part],
                                complete,
                            ) {
                                return Err(malformed("separators out of order"));
                            }
                        }
                        previous = Some((sep_at, part));
                        refs += n;
                    }
                    slots.push(slot(at, first_ref)?);
                    at = entry_end;
                }
            }
            PageKind::Overflow => {
                if h.level != 0 || h.refs != 0 || h.count == 0 || h.count != end - BODY_AT {
                    return Err(malformed("an overflow page header"));
                }
                at = end;
            }
        }
        if at != end {
            return Err(malformed("bytes after the last item"));
        }
        if refs != h.refs {
            return Err(malformed("the number of page references"));
        }
        Ok(Page {
            number,
            hash,
            bytes,
            kind: h.kind,
            level: h.level,
            written: h.written,
            count: h.count,
            region_len: h.region_len,
            refs: h.refs,
            slots,
        })
    }

    /// The page's number.
    pub fn number(&self) -> PageNo {
        self.number
    }

    /// The page's hash: SHA-256 over its region, which holds everything in
    /// the page except page numbers and the checksum.
    pub fn hash(&self) -> Hash {
        self.hash
    }

    pub fn kind(&self) -> PageKind {
        self.kind
    }

    /// 0 for leaves and overflow pages; an index page is one above its
    /// children.
    pub fn level(&self) -> u8 {
        self.level
    }

    /// The index of the entry that wrote the page.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// The number of items (leaf), children (index) or data bytes
    /// (overflow).
    pub fn count(&self) -> usize {
        self.count
    }

    /// The page's bytes, as stored.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Every page this page refers to, in order: an index page's children,
    /// then the overflow pages of its separators; a leaf's overflow pages.
    /// What the page file's space accounting follows.
    pub fn references(&self) -> Vec<(PageNo, Hash)> {
        let mut out = Vec::with_capacity(self.refs);
        match self.kind {
            PageKind::Leaf => {
                for i in 0..self.count {
                    out.extend(self.item(i).refs.iter());
                }
            }
            PageKind::Index => {
                for i in 0..self.count {
                    let child = self.child(i);
                    out.push((child.number, child.hash));
                }
                for i in 1..self.count {
                    if let Some(separator) = self.child(i).separator {
                        out.extend(separator.refs.iter());
                    }
                }
            }
            PageKind::Overflow => {}
        }
        out
    }

    fn pointers(&self) -> &[u8] {
        let at = REGION_AT + self.region_len;
        &self.bytes[at..at + self.refs * 8]
    }

    fn refs_at(&self, hashes_at: usize, first: usize, n: usize) -> Refs<'_> {
        Refs {
            hashes: &self.bytes[hashes_at..hashes_at + n * HASH_LEN],
            numbers: &self.pointers()[first * 8..(first + n) * 8],
        }
    }

    /// Item `i` of a leaf page.
    pub(crate) fn item(&self, i: usize) -> ItemView<'_> {
        debug_assert_eq!(self.kind, PageKind::Leaf);
        let Slot { at, first_ref } = self.slots[i];
        let at = usize::from(at);
        let key_len = u16_at(&self.bytes, at + 1);
        let value_len = u16_at(&self.bytes, at + 3);
        let (inline, n) = match item_inline(key_len, value_len) {
            Some(inline) => (inline, overflow_pages(key_len + value_len)),
            None => (key_len + value_len, 0),
        };
        let inline_at = at + ITEM_HEADER;
        ItemView {
            kind: self.bytes[at],
            key_len,
            value_len,
            inline: &self.bytes[inline_at..inline_at + inline],
            refs: self.refs_at(inline_at + inline, usize::from(first_ref), n),
        }
    }

    /// Child entry `i` of an index page.
    pub(crate) fn child(&self, i: usize) -> ChildView<'_> {
        debug_assert_eq!(self.kind, PageKind::Index);
        let Slot { at, first_ref } = self.slots[i];
        let at = usize::from(at);
        let separator = (i > 0).then(|| {
            let len_at = at + HASH_LEN + 8;
            let len = u16_at(&self.bytes, len_at);
            let (inline, n) = match separator_inline(len) {
                Some(inline) => (inline, overflow_pages(len)),
                None => (len, 0),
            };
            let inline_at = len_at + SEPARATOR_HEADER;
            SeparatorView {
                len,
                inline: &self.bytes[inline_at..inline_at + inline],
                refs: self.refs_at(inline_at + inline, usize::from(first_ref), n),
            }
        });
        ChildView {
            number: u64_at(self.pointers(), i * 8),
            hash: hash_at(&self.bytes, at),
            stamp: u64_at(&self.bytes, at + HASH_LEN),
            separator,
        }
    }

    /// The data of an overflow page.
    pub(crate) fn data(&self) -> &[u8] {
        debug_assert_eq!(self.kind, PageKind::Overflow);
        &self.bytes[BODY_AT..REGION_AT + self.region_len]
    }
}

fn slot(at: usize, first_ref: usize) -> Result<Slot, Fault> {
    match (u16::try_from(at), u16::try_from(first_ref)) {
        (Ok(at), Ok(first_ref)) => Ok(Slot { at, first_ref }),
        _ => Err(Fault::Malformed("an offset beyond the page")),
    }
}

/// Whether a key known as far as `a` can sort strictly before a key known
/// as far as `b` (all of it when `b_complete`). False only when the known
/// parts prove it does not.
pub(crate) fn in_order(a: &[u8], b: &[u8], b_complete: bool) -> bool {
    let n = a.len().min(b.len());
    match a[..n].cmp(&b[..n]) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        // `b` complete and a prefix of what is known of `a`: `a >= b`.
        std::cmp::Ordering::Equal => !(b_complete && b.len() <= a.len()),
    }
}

/// A separator as it goes into a page: its length, the bytes the page holds
/// (all of it, or the first [`SPILL_INLINE`]) and its overflow pages.
pub(crate) type Separator<'a> = (usize, &'a [u8], &'a [(PageNo, Hash)]);

/// Builds the bytes of a new page.
pub(crate) struct Builder {
    region: Vec<u8>,
    children: Vec<PageNo>,
    overflow: Vec<PageNo>,
    count: usize,
    kind: PageKind,
}

impl Builder {
    pub(crate) fn new(kind: PageKind, level: u8, written: u64) -> Builder {
        let mut region = Vec::with_capacity(PAGE_SIZE - REGION_AT);
        region.extend_from_slice(&[FORMAT_VERSION, kind as u8, level, 0]);
        region.resize(16, 0);
        region.extend_from_slice(&written.to_le_bytes());
        Builder {
            region,
            children: Vec::new(),
            overflow: Vec::new(),
            count: 0,
            kind,
        }
    }

    fn refs(&mut self, refs: &[(PageNo, Hash)]) {
        for (number, hash) in refs {
            self.region.extend_from_slice(&hash.0);
            self.overflow.push(*number);
        }
    }

    /// Appends a leaf item: `inline` is the key and value whole, or their
    /// first [`SPILL_INLINE`] bytes with `refs` naming the rest.
    pub(crate) fn item(
        &mut self,
        kind: u8,
        key_len: usize,
        value_len: usize,
        inline: &[u8],
        refs: &[(PageNo, Hash)],
    ) {
        debug_assert_eq!(self.kind, PageKind::Leaf);
        self.region.push(kind);
        self.region
            .extend_from_slice(&(key_len as u16).to_le_bytes());
        self.region
            .extend_from_slice(&(value_len as u16).to_le_bytes());
        self.region.extend_from_slice(inline);
        self.refs(refs);
        self.count += 1;
    }

    /// Appends a child entry, with its separator unless it is the first.
    pub(crate) fn child(
        &mut self,
        number: PageNo,
        hash: &Hash,
        stamp: u64,
        separator: Option<Separator<'_>>,
    ) {
        debug_assert_eq!(self.kind, PageKind::Index);
        self.region.extend_from_slice(&hash.0);
        self.region.extend_from_slice(&stamp.to_le_bytes());
        if let Some((len, inline, refs)) = separator {
            self.region.extend_from_slice(&(len as u16).to_le_bytes());
            self.region.extend_from_slice(inline);
            self.refs(refs);
        }
        self.children.push(number);
        self.count += 1;
    }

    /// Sets the data of an overflow page.
    pub(crate) fn data(&mut self, data: &[u8]) {
        debug_assert_eq!(self.kind, PageKind::Overflow);
        self.region.extend_from_slice(data);
        self.count = data.len();
    }

    /// The page, as page `number`. Fails only if the content does not fit,
    /// which the tree's footprint accounting rules out.
    pub(crate) fn finish(mut self, number: PageNo) -> Result<Page, Fault> {
        let refs = self.children.len() + self.overflow.len();
        let region_len = self.region.len();
        if REGION_AT + region_len + refs * 8 > PAGE_SIZE || self.count > usize::from(u16::MAX) {
            return Err(Fault::Malformed("content larger than a page"));
        }
        self.region[4..6].copy_from_slice(&(self.count as u16).to_le_bytes());
        self.region[6..8].copy_from_slice(&(region_len as u16).to_le_bytes());
        self.region[8..10].copy_from_slice(&(refs as u16).to_le_bytes());
        let mut bytes = vec![0u8; PAGE_SIZE].into_boxed_slice();
        bytes[NUMBER_AT..REGION_AT].copy_from_slice(&number.to_le_bytes());
        bytes[REGION_AT..REGION_AT + region_len].copy_from_slice(&self.region);
        let mut at = REGION_AT + region_len;
        for n in self.children.iter().chain(&self.overflow) {
            bytes[at..at + 8].copy_from_slice(&n.to_le_bytes());
            at += 8;
        }
        let header = Header::read(&bytes).ok_or(Fault::Malformed("a header"))?;
        let hash = Hash::of(&self.region);
        let sum = checksum(&hash, &bytes, &header);
        bytes[..8].copy_from_slice(&sum.to_le_bytes());
        Page::parse(number, hash, bytes, header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf_with_overflow() -> (Page, Page) {
        let mut over = Builder::new(PageKind::Overflow, 0, 7);
        over.data(&[0x33; 3500]);
        let over = over.finish(90).unwrap();
        let mut leaf = Builder::new(PageKind::Leaf, 0, 7);
        leaf.item(KIND_VALUE, 3, 2, b"a\x00\x01xy", &[]);
        // A spilled item: 1024 inline bytes, the rest in `over`.
        let mut inline = b"b\x00\x01".to_vec();
        inline.resize(SPILL_INLINE, 0x33);
        leaf.item(
            KIND_BOTH,
            3,
            SPILL_INLINE - 3 + 3500,
            &inline,
            &[(90, over.hash())],
        );
        (leaf.finish(12).unwrap(), over)
    }

    #[test]
    fn a_built_page_verifies_and_reads_back() {
        let (leaf, over) = leaf_with_overflow();
        let again = Page::verify(12, &leaf.hash(), leaf.bytes().into()).unwrap();
        assert_eq!(again.count(), 2);
        assert_eq!(again.item(0).value_inline(), Some(&b"xy"[..]));
        assert_eq!(again.item(1).refs.get(0), (90, over.hash()));
        assert_eq!(again.references(), vec![(90, over.hash())]);
        let over2 = Page::verify(90, &over.hash(), over.bytes().into()).unwrap();
        assert_eq!(over2.data().len(), 3500);
    }

    /// Every single-byte change to a page is found, whichever byte it is:
    /// the checksum, the page's number, the region, the pointer array or
    /// the zero fill.
    #[test]
    fn every_damaged_byte_is_found() {
        let (leaf, _) = leaf_with_overflow();
        let mut mask = 0x9eu8;
        for at in 0..PAGE_SIZE {
            let mut bytes: Box<[u8]> = leaf.bytes().into();
            mask = mask.rotate_left(3) | 1;
            bytes[at] ^= mask;
            let result = Page::verify(12, &leaf.hash(), bytes);
            assert!(result.is_err(), "byte {at} changed and not found");
        }
    }

    #[test]
    fn a_page_at_the_wrong_place_or_of_another_version_is_named_as_such() {
        let (leaf, over) = leaf_with_overflow();
        assert_eq!(
            Page::verify(13, &leaf.hash(), leaf.bytes().into()).unwrap_err(),
            Fault::Misplaced { found: 12 }
        );
        assert_eq!(
            Page::verify(12, &over.hash(), leaf.bytes().into()).unwrap_err(),
            Fault::Stale { found: leaf.hash() }
        );
    }

    /// Review of #697: a stored key deeper than 64 segments is malformed,
    /// whether the page holds it whole or only its start.
    #[test]
    fn a_stored_key_beyond_the_depth_bound_is_malformed() {
        let segments = |n: usize| b"\x00\x01".repeat(n);
        let leaf = |key: &[u8], value_len: usize| {
            let mut builder = Builder::new(PageKind::Leaf, 0, 1);
            let mut inline = key.to_vec();
            inline.resize(key.len() + value_len, 0x33);
            let (inline, refs) = match item_inline(key.len(), value_len) {
                Some(n) => (&inline[..n], vec![(5, Hash([9; 32]))]),
                None => (&inline[..], vec![]),
            };
            builder.item(KIND_VALUE, key.len(), value_len, inline, &refs);
            builder.finish(1)
        };
        assert!(leaf(&segments(64), 1).is_ok());
        assert_eq!(
            leaf(&segments(65), 1).unwrap_err(),
            Fault::Malformed("a key that is not an encoded path")
        );
        // A spilled item whose page holds 64 whole segments and more.
        assert!(leaf(&segments(64), 5000).is_ok());
        let mut deep = segments(64);
        deep.extend(b"x".repeat(2000));
        deep.extend(b"\x00\x01");
        assert_eq!(
            leaf(&deep, 10).unwrap_err(),
            Fault::Malformed("a key that is not an encoded path")
        );
    }

    /// Re-check of #697: a separator must be able to begin a path within the
    /// bounds, whether its page holds it whole or only its start.
    #[test]
    fn a_separator_beyond_the_depth_bound_is_malformed() {
        let index = |sep: &[u8], refs: &[(PageNo, Hash)]| {
            let mut builder = Builder::new(PageKind::Index, 1, 1);
            builder.child(10, &Hash([1; 32]), 1, None);
            let inline = separator_inline(sep.len()).unwrap_or(sep.len());
            builder.child(
                11,
                &Hash([2; 32]),
                1,
                Some((sep.len(), &sep[..inline], refs)),
            );
            builder.finish(1)
        };
        let ends = |n: usize| b"\x00\x01".repeat(n);
        assert!(index(&ends(64), &[]).is_ok());
        assert!(index(b"a\x00\x01\x00", &[]).is_ok());
        let malformed = Fault::Malformed("a separator that begins no path");
        assert_eq!(index(&ends(65), &[]).unwrap_err(), malformed);
        // Spilled: its page holds 64 segment ends and more follows.
        let mut long = ends(64);
        long.resize(5000, b'x');
        assert_eq!(index(&long, &[(12, Hash([3; 32]))]).unwrap_err(), malformed);
    }

    #[test]
    fn the_hash_does_not_cover_page_numbers() {
        let mut a = Builder::new(PageKind::Index, 1, 5);
        a.child(100, &Hash([1; 32]), 5, None);
        a.child(101, &Hash([2; 32]), 4, Some((1, b"m", &[])));
        let mut b = Builder::new(PageKind::Index, 1, 5);
        b.child(7, &Hash([1; 32]), 5, None);
        b.child(8, &Hash([2; 32]), 4, Some((1, b"m", &[])));
        let (a, b) = (a.finish(1).unwrap(), b.finish(2).unwrap());
        assert_eq!(a.hash(), b.hash());
        assert_ne!(a.bytes()[..8], b.bytes()[..8], "the checksums differ");
    }
}
